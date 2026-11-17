//! Sockets and TLS: the primitives behind norn.net.
//!
//! TCP and UDP sockets are non-blocking `mio` sources on the reactor; only
//! name resolution runs on the blocking pool. TLS (rustls, with the `ring`
//! provider) wraps a connected TCP stream. A TLS operation may need the
//! other direction of the socket first (a read can owe handshake records, a
//! write can wait for the peer's), so it answers `:again` (wait until
//! readable) or `:again-write` (wait until writable), and the wrapper waits
//! on the direction it names.

use crate::reactor::{self, Io};
use crate::vm::{Error, Result, Vm};
use norn_rt::runtime::rt;
use norn_rt::string::{bytes_new, str_new};
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Mutator, Value};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

fn kw(m: &mut Mutator, k: &str) -> Value {
    rt().keyword(m, None, k)
}

fn io_err(e: std::io::Error) -> Error {
    Error::io(e.to_string(), &e)
}

/// A TLS stream's error: rustls's own (a bad certificate, a broken record)
/// arrive as InvalidData, and are TlsErrors; the rest are I/O errors.
pub(crate) fn tls_error(what: &str, e: &std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::InvalidData {
        Error::of("TlsError", format!("{what}: {e}")).with("reason", crate::vm::ErrData::Kw("tls"))
    } else {
        Error::io(format!("{what}: {e}"), e)
    }
}

/// Handle h's "host:port", from its info, for an error's :address.
fn address_of(m: &mut Mutator, h: Value) -> Option<String> {
    let info = h.field(norn_rt::conc::handle::INFO);
    if !info.is(Kind::Map) {
        return None;
    }
    let (kh, kp) = (kw(m, "host"), kw(m, "port"));
    let host = info.map_get(kh).filter(|v| v.is(Kind::String))?;
    let port = info.map_get(kp).filter(|v| v.is_fixnum())?;
    Some(format!("{}:{}", norn_rt::print::pr_str(host, false), port.as_fixnum()))
}

fn parse_addr(s: &str) -> Result<SocketAddr> {
    s.parse().map_err(|_| Error::new(format!("Not a socket address (ip:port): {s}")))
}

/// Raise the open-file limit to the most the OS allows, as Go does: one
/// process may hold many thousands of sockets.
pub(crate) fn raise_fd_limit() {
    // SAFETY: getrlimit/setrlimit only read and write the struct.
    unsafe {
        let mut r: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) != 0 {
            return;
        }
        let want = r.rlim_max;
        // macOS refuses RLIM_INFINITY; its real cap is kern.maxfilesperproc.
        #[cfg(target_os = "macos")]
        let want = {
            let mut cap: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>();
            if libc::sysctlbyname(c"kern.maxfilesperproc".as_ptr(), &mut cap as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) == 0 {
                want.min(cap as libc::rlim_t)
            } else {
                want
            }
        };
        if want > r.rlim_cur {
            r.rlim_cur = want;
            libc::setrlimit(libc::RLIMIT_NOFILE, &r);
        }
    }
}

// ---- names --------------------------------------------------------------------------------

/// `(%resolve host port)`: the socket addresses of a host name, as
/// "ip:port" strings (on the blocking pool: DNS can take seconds).
pub(crate) fn resolve(vm: &mut Vm, host: String, port: u16) -> Result<Value> {
    crate::blocking::run(vm, move || {
        use std::net::ToSocketAddrs;
        let r = (host.as_str(), port).to_socket_addrs().map(|a| a.map(|a| a.to_string()).collect::<Vec<_>>()).map_err(|e| {
            crate::vm::Failure::of("ConnectionError", format!("Can't resolve {host}: {e}"))
                .with("address", crate::vm::FailData::Str(host.clone()))
                .with("reason", crate::vm::FailData::Kw("host-not-found"))
        });
        Box::new(move |m: &mut Mutator| {
            let addrs = r?;
            let vs: Vec<Value> = addrs.iter().map(|a| str_new(m, a)).collect();
            Ok(vec_from_slice(m, &vs))
        }) as crate::blocking::Finish
    })
}

// ---- TCP ----------------------------------------------------------------------------------------

/// Start connecting to `addr` (non-blocking): a `:tcp` handle that
/// `%tcp-check` reports on.
pub(crate) fn tcp_connect(vm: &mut Vm, addr: &str, info: Value) -> Result<Value> {
    let s = mio::net::TcpStream::connect(parse_addr(addr)?)
        .map_err(|e| Error::io(format!("Can't connect to {addr}: {e}"), &e).with("address", crate::vm::ErrData::Str(addr.to_owned())))?;
    reactor::register(&mut vm.m, Io::Tcp(s), "tcp", info).map_err(Error::new)
}

/// Is the connection of a `:tcp` handle up? true, :again-write (still
/// connecting), or an error (refused, unreachable…).
pub(crate) fn tcp_check(vm: &mut Vm, h: Value) -> Result<Value> {
    let r = reactor::with_io(h, |io| match io {
        Io::Tcp(s) => {
            if let Some(e) = s.take_error()? {
                return Err(e);
            }
            match s.peer_addr() {
                Ok(_) => Ok(true),
                // (The connect may fail between the two questions: macOS
                // then answers peer_addr with EINVAL, not ENOTCONN. The
                // socket's error, asked again, says why: refused, say.)
                Err(e) => match s.take_error()? {
                    Some(why) => Err(why),
                    None if e.kind() == std::io::ErrorKind::NotConnected => Ok(false),
                    None => Err(e),
                },
            }
        }
        _ => Err(std::io::Error::other("not a TCP stream")),
    })?;
    match r {
        Ok(true) => {
            let _ = reactor::with_io(h, |io| if let Io::Tcp(s) = io { s.set_nodelay(true) } else { Ok(()) });
            Ok(Value::TRUE)
        }
        Ok(false) => Ok(kw(&mut vm.m, "again-write")),
        Err(e) => {
            let mut err = Error::io(format!("Can't connect: {e}"), &e);
            if let Some(a) = address_of(&mut vm.m, h) {
                err = err.with("address", crate::vm::ErrData::Str(a));
            }
            Err(err)
        }
    }
}

/// Listen on `addr`: a `:tcp-listener` handle.
pub(crate) fn tcp_listen(vm: &mut Vm, addr: &str, info: Value) -> Result<Value> {
    let l = mio::net::TcpListener::bind(parse_addr(addr)?)
        .map_err(|e| Error::io(format!("Can't listen on {addr}: {e}"), &e).with("address", crate::vm::ErrData::Str(addr.to_owned())))?;
    reactor::register(&mut vm.m, Io::TcpListener(l), "tcp-listener", info).map_err(Error::new)
}

/// Accept a connection: a `:tcp` handle, or :again.
pub(crate) fn tcp_accept(vm: &mut Vm, h: Value) -> Result<Value> {
    let r = reactor::with_io(h, |io| match io {
        Io::TcpListener(l) => l.accept(),
        _ => Err(std::io::Error::other("not a TCP listener")),
    })?;
    match r {
        Ok((s, peer)) => {
            let _ = s.set_nodelay(true);
            let p = str_new(&mut vm.m, &peer.to_string());
            let k = kw(&mut vm.m, "peer");
            let info = norn_rt::map::map_assoc(&mut vm.m, norn_rt::map::map_empty(), k, p);
            reactor::register(&mut vm.m, Io::Tcp(s), "tcp", info).map_err(Error::new)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(kw(&mut vm.m, "again")),
        Err(e) => Err(Error::io(format!("accept failed: {e}"), &e)),
    }
}

/// The local or peer address of a socket, as "ip:port".
pub(crate) fn address(vm: &mut Vm, h: Value, peer: bool) -> Result<Value> {
    let a = reactor::with_io(h, |io| match io {
        Io::Tcp(s) => if peer { s.peer_addr() } else { s.local_addr() },
        Io::TcpListener(l) if !peer => l.local_addr(),
        Io::Udp(s) => if peer { s.peer_addr() } else { s.local_addr() },
        Io::Tls(t) => if peer { t.sock.peer_addr() } else { t.sock.local_addr() },
        _ => Err(std::io::Error::other("not a socket")),
    })?
    .map_err(io_err)?;
    Ok(str_new(&mut vm.m, &a.to_string()))
}

/// Stop sending (the peer reads end of input); reading still works.
pub(crate) fn shutdown_write(h: Value) -> Result<Value> {
    reactor::with_io(h, |io| match io {
        Io::Tcp(s) => s.shutdown(std::net::Shutdown::Write),
        Io::Tls(t) => {
            t.conn.send_close_notify();
            let _ = t.flush();
            t.sock.shutdown(std::net::Shutdown::Write)
        }
        _ => Err(std::io::Error::other("not a stream socket")),
    })?
    .map_err(io_err)?;
    Ok(Value::NIL)
}

// ---- UDP ----------------------------------------------------------------------------------------

pub(crate) fn udp_bind(vm: &mut Vm, addr: &str, info: Value) -> Result<Value> {
    let s = mio::net::UdpSocket::bind(parse_addr(addr)?).map_err(|e| Error::new(format!("Can't bind {addr}: {e}")))?;
    reactor::register(&mut vm.m, Io::Udp(s), "udp", info).map_err(Error::new)
}

/// Send one datagram: bytes sent, or :again-write.
pub(crate) fn udp_send(vm: &mut Vm, h: Value, data: &[u8], to: &str) -> Result<Value> {
    let to = parse_addr(to)?;
    let r = reactor::with_io(h, |io| match io {
        Io::Udp(s) => s.send_to(data, to),
        _ => Err(std::io::Error::other("not a UDP socket")),
    })?;
    match r {
        Ok(n) => Ok(Value::fix(n as i64)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(kw(&mut vm.m, "again-write")),
        Err(e) => Err(Error::io(format!("send failed: {e}"), &e)),
    }
}

/// Receive one datagram (up to n bytes): `[bytes "ip:port"]`, or :again.
pub(crate) fn udp_recv(vm: &mut Vm, h: Value, n: usize) -> Result<Value> {
    let mut buf = vec![0u8; n.clamp(1, 65536)];
    let r = reactor::with_io(h, |io| match io {
        Io::Udp(s) => s.recv_from(&mut buf),
        _ => Err(std::io::Error::other("not a UDP socket")),
    })?;
    match r {
        Ok((k, from)) => {
            let b = bytes_new(&mut vm.m, &buf[..k]);
            let f = str_new(&mut vm.m, &from.to_string());
            Ok(vec_from_slice(&mut vm.m, &[b, f]))
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(kw(&mut vm.m, "again")),
        Err(e) => Err(Error::io(format!("receive failed: {e}"), &e)),
    }
}

// ---- TLS ------------------------------------------------------------------------------------------

pub(crate) struct Tls {
    pub sock: mio::net::TcpStream,
    pub conn: rustls::Connection,
}

/// What a TLS operation needs before it can go on.
pub(crate) enum Wait {
    Read,
    Write,
}

impl Tls {
    /// Send buffered TLS records. Err(WouldBlock) if the socket is full.
    fn flush(&mut self) -> std::io::Result<()> {
        while self.conn.wants_write() {
            self.conn.write_tls(&mut self.sock)?;
        }
        Ok(())
    }

    /// Read TLS records from the socket and process them. Ok(false) at
    /// end of input.
    fn fill(&mut self) -> std::io::Result<bool> {
        let n = self.conn.read_tls(&mut self.sock)?;
        self.conn.process_new_packets().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(n > 0)
    }

    /// Up to `n` bytes of plaintext: Ok(Some), Ok(None) at the end, or a wait.
    pub(crate) fn read(&mut self, n: usize) -> std::result::Result<Option<Vec<u8>>, std::result::Result<Wait, std::io::Error>> {
        let mut buf = vec![0u8; n.clamp(1, 1 << 20)];
        loop {
            match self.conn.reader().read(&mut buf) {
                Ok(0) => return Ok(None),
                Ok(k) => {
                    buf.truncate(k);
                    return Ok(Some(buf));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                // The peer closed without a close_notify: treat as the end.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(Err(e)),
            }
            match self.flush() {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(Ok(Wait::Write)),
                Err(e) => return Err(Err(e)),
            }
            match self.fill() {
                Ok(true) => {}
                Ok(false) => return Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(Ok(Wait::Read)),
                Err(e) => return Err(Err(e)),
            }
        }
    }

    /// Write some of `data`: the number of bytes taken, or a wait.
    pub(crate) fn write(&mut self, data: &[u8]) -> std::result::Result<usize, std::result::Result<Wait, std::io::Error>> {
        loop {
            match self.flush() {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(Ok(Wait::Write)),
                Err(e) => return Err(Err(e)),
            }
            if !self.conn.is_handshaking() {
                break;
            }
            // The handshake needs the peer's records before data can go.
            match self.fill() {
                Ok(true) => {}
                Ok(false) => return Err(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed during the TLS handshake"))),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(Ok(Wait::Read)),
                Err(e) => return Err(Err(e)),
            }
        }
        let n = self.conn.writer().write(data).map_err(Err)?;
        if n == 0 && !data.is_empty() {
            return Err(Ok(Wait::Write));
        }
        match self.flush() {
            Ok(()) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(n),
            Err(e) => Err(Err(e)),
        }
    }

    /// Drive the handshake to its end: Ok, or a wait. A certificate the
    /// client doesn't trust fails here.
    pub(crate) fn handshake(&mut self) -> std::result::Result<(), std::result::Result<Wait, std::io::Error>> {
        while self.conn.is_handshaking() {
            match self.flush() {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(Ok(Wait::Write)),
                Err(e) => return Err(Err(e)),
            }
            if !self.conn.is_handshaking() {
                break;
            }
            match self.fill() {
                Ok(true) => {}
                Ok(false) => return Err(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed during the TLS handshake"))),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(Ok(Wait::Read)),
                Err(e) => return Err(Err(e)),
            }
        }
        self.drain()
    }

    /// Finish sending what is buffered: Ok, or a wait for writability.
    pub(crate) fn drain(&mut self) -> std::result::Result<(), std::result::Result<Wait, std::io::Error>> {
        match self.flush() {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(Ok(Wait::Write)),
            Err(e) => Err(Err(e)),
        }
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static P: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    P.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())).clone()
}

fn pem_certs(pem: &[u8]) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;
    rustls::pki_types::CertificateDer::pem_slice_iter(pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::new(format!("Bad certificate PEM: {e:?}")))
}

/// The OS's trusted root certificates (loaded once).
fn native_roots() -> Arc<rustls::RootCertStore> {
    static R: OnceLock<Arc<rustls::RootCertStore>> = OnceLock::new();
    R.get_or_init(|| {
        let mut store = rustls::RootCertStore::empty();
        for c in rustls_native_certs::load_native_certs().certs {
            let _ = store.add(c);
        }
        Arc::new(store)
    })
    .clone()
}

/// Move the TCP stream of handle `h` into a new TLS handle (the old handle
/// is closed).
fn wrap(vm: &mut Vm, h: Value, conn: rustls::Connection, info: Value) -> Result<Value> {
    let e = reactor::entry_of(h, "tcp")?;
    let sock = match e.io.lock().unwrap().take() {
        Some(Io::Tcp(s)) => s,
        _ => return Err(Error::new("The TCP handle is closed")),
    };
    reactor::close(h)?;
    let mut sock = sock;
    // Re-registering the same socket under a new handle number.
    let _ = reactor::deregister_source(&mut sock);
    reactor::register(&mut vm.m, Io::Tls(Box::new(Tls { sock, conn })), "tls", info).map_err(Error::new)
}

/// `(%tls-client tcp server-name ca-pem-or-nil)`: start TLS as a client.
pub(crate) fn tls_client(vm: &mut Vm, h: Value, server_name: &str, ca_pem: Option<&[u8]>, info: Value) -> Result<Value> {
    let roots = match ca_pem {
        Some(pem) => {
            let mut store = rustls::RootCertStore::empty();
            for c in pem_certs(pem)? {
                store.add(c).map_err(|e| Error::new(format!("Bad CA certificate: {e}")))?;
            }
            Arc::new(store)
        }
        None => native_roots(),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::new(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(server_name.to_owned()).map_err(|e| Error::new(format!("Bad server name {server_name}: {e}")))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| Error::new(e.to_string()))?;
    wrap(vm, h, conn.into(), info)
}

/// `(%tls-server tcp cert-pem key-pem)`: start TLS as a server.
pub(crate) fn tls_server(vm: &mut Vm, h: Value, cert_pem: &[u8], key_pem: &[u8], info: Value) -> Result<Value> {
    use rustls::pki_types::pem::PemObject;
    let certs = pem_certs(cert_pem)?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| Error::new(format!("Bad private key PEM: {e:?}")))?;
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::new(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| Error::new(format!("Bad certificate or key: {e}")))?;
    let conn = rustls::ServerConnection::new(Arc::new(config)).map_err(|e| Error::new(e.to_string()))?;
    wrap(vm, h, conn.into(), info)
}
