//! I/O runtime checks that depend on timing, so each runs in a `norn`
//! process of its own with a known number of carriers (the shared suite
//! runner keeps every carrier busy with other suites).

use std::process::Command;

fn norn(threads: &str, expr: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_norn")).args(["-e", expr]).env("NORN_THREADS", threads).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn the_blocking_pool_runs_calls_side_by_side() {
    // 16 calls blocking 300 ms each: 4.8 s one after another, 0.3 s side by
    // side (the pool grows to one thread per waiting call, not just when
    // none is idle).
    let r = norn(
        "2",
        "(let [t0 (nano-time)
               r (mapv await (mapv (fn [_] (spawn-detached (%blocking-sleep 300))) (range 16)))]
           [(= r (vec (repeat 16 300))) (< (- (nano-time) t0) 1200000000)])",
    );
    assert_eq!(r, "[true true]");
}

#[test]
fn a_spare_carrier_runs_fibers_while_carriers_are_stuck_in_native_code() {
    // Both carriers block for 2 s in native code; the worker still finishes
    // first, on a spare carrier, which retires afterwards.
    let r = norn(
        "2",
        "(let [stalls (mapv (fn [_] (spawn-detached (%native-stall 2000))) (range 2))
               _ (sleep 50)
               worker (spawn-detached (reduce + (range 100000)))
               v (await worker)
               early (if (some (fn [f] (not (done? f))) stalls) true false)
               spares (:carrier-threads (%io-stats))]
           (mapv await stalls)
           (sleep 400)
           [v early (> spares 2) (:carrier-threads (%io-stats))])",
    );
    assert_eq!(r, "[4999950000 true true 2]");
}

#[test]
fn a_slow_fast_ffi_call_gets_a_spare_carrier() {
    // usleep declared as a fast (non-blocking) call: both carriers sit in C
    // for 1.5 s, and the worker still runs on a spare carrier.
    let r = norn(
        "2",
        r#"(do (require '[norn.ffi :as ffi])
               (ffi/defcfun slow-sleep (ffi/library :c) "usleep" [:uint] :int)
               (let [stalls (mapv (fn [_] (spawn-detached (slow-sleep 1500000))) (range 2))
                     _ (sleep 50)
                     worker (spawn-detached (reduce + (range 100000)))
                     v (await worker)
                     early (if (some (fn [f] (not (done? f))) stalls) true false)]
                 [v early (mapv await stalls)]))"#,
    );
    assert_eq!(r, "[4999950000 true [0 0]]");
}

#[test]
fn a_collection_during_a_native_call_spares_the_cached_segment() {
    // A carrier keeps one free stack segment cached, and it isn't a GC root.
    // A fiber that has just come back across a segment boundary (so one is
    // cached) enters a native call; another collects meanwhile. The cached
    // segment must have been dropped first: else it's freed, and the next
    // deep call writes its frames over whatever is put there (M9: SQLite's
    // suite crashed the process this way, 1 run in 10).
    let r = norn(
        "2",
        "(do (defn deep [n] (if (zero? n) 0 (inc (deep (dec n)))))
             (let [a (spawn-detached (dotimes [_ 40] (deep 30000) (%native-stall 15) (deep 30000)) :a)
                   b (spawn-detached (dotimes [_ 400] (gc) (count (pr-str (vec (range 3000))))) :b)]
               [(await a) (await b)]))",
    );
    assert_eq!(r, "[:a :b]");
}

// ---- M6 done-check ------------------------------------------------------------------------

#[test]
fn an_echo_server_holds_10000_connections_on_two_carriers() {
    // Every connection is open at the same moment (the server counts them),
    // and two carriers serve them all: a connection never blocks a carrier.
    let r = norn(
        "2",
        r#"(do (require '[norn.net :as net] '[norn.io :as io])
               (def n 10000)
               (def l (net/listen "127.0.0.1" 0))
               (def port (net/port (net/local-address l)))
               (def open (atom 0))
               (def accepted (atom 0))
               (def peak (atom 0))
               (def all-in (chan))
               (def server
                 (spawn-detached
                   (dotimes [_ n]
                     (let [c (net/accept l)]
                       (swap! accepted inc)
                       (spawn-detached
                         (let [now (swap! open inc)]
                           (swap! peak max now)
                           (when (= now n) (close! all-in))
                           (let [line (io/read-line (io/line-reader c))]
                             (io/write c (str line "!\n"))
                             ;; hold until all are connected (or give up)
                             (alts! [all-in (timeout 120000)])
                             (io/close c)                 ; the server closes first
                             (swap! open dec))))))))
               (defn talk [i c]
                 (let [r (io/line-reader c)]
                   (io/write c (str i "\n"))
                   (let [reply (io/read-line r)
                         eof (io/read-line r)]
                     (io/close c)
                     (and (= reply (str i "!")) (nil? eof)))))
               ;; Connect in batches of 100, each accepted by the server before the
               ;; next (macOS caps the accept backlog at 128, and a connection
               ;; is established before it's accepted), then talk on all.
               (def clients
                 (loop [i 0 acc []]
                   (if (< i n)
                     (let [conns (mapv await (mapv (fn [_] (spawn-detached (net/connect "127.0.0.1" port))) (range 100)))]
                       (loop [] (when (< @accepted (+ i 100)) (sleep 1) (recur)))
                       (recur (+ i 100) (into acc (map-indexed (fn [j c] (spawn-detached (talk (+ i j) c))) conns))))
                     acc)))
               (def ok (count (filter true? (mapv await clients))))
               (await server)
               [ok @peak (carrier-count)])"#,
    );
    assert_eq!(r, "[10000 10000 2]");
}

#[test]
fn a_socket_saved_in_an_image_is_lost_until_reconnected() {
    use std::io::BufRead;
    let dir = std::env::temp_dir().join(format!("norn-io-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let img = dir.join("socket.nimg");
    // An echo server in a process of its own, printing its port.
    let mut server = Command::new(env!("CARGO_BIN_EXE_norn"))
        .args([
            "-e",
            r#"(do (require '[norn.net :as net] '[norn.io :as io])
                   (let [l (net/listen "127.0.0.1" 0)]
                     (println (net/port (net/local-address l)))
                     (loop []
                       (let [c (net/accept l)]
                         (spawn-detached
                           (let [r (io/line-reader c)]
                             (loop [] (when-let [line (io/read-line r)] (io/write c (str line "!\n")) (recur)))
                             (io/close c))))
                       (recur))))"#,
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut port = String::new();
    std::io::BufReader::new(server.stdout.take().unwrap()).read_line(&mut port).unwrap();
    let port = port.trim().to_owned();

    // A client connects, talks, and saves an image with the socket in it.
    let saved = norn(
        "2",
        &format!(
            r#"(do (require '[norn.net :as net] '[norn.io :as io])
                   (def c (net/connect "127.0.0.1" {port}))
                   (io/write c "before\n")
                   (def reply (io/read-line (io/line-reader c)))
                   (save-image "{}")
                   reply)"#,
            img.display()
        ),
    );
    assert_eq!(saved, "\"before!\"");

    // Restored: the socket is lost; the reconnect restart connects anew.
    let out = Command::new(env!("CARGO_BIN_EXE_norn"))
        .args([
            "--image",
            img.to_str().unwrap(),
            "-e",
            r#"[(try (norn.io/write c "x\n") :no-error (catch ResourceLost e :lost))
                (handler-bind [ResourceLost (fn [e] (invoke-restart 'reconnect))]
                  (let [c2 (ensure-live c)]
                    (norn.io/write c2 "after\n")
                    (norn.io/read-line (norn.io/line-reader c2))))]"#,
        ])
        .output()
        .unwrap();
    let _ = server.kill();
    let _ = server.wait();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(text, r#"[:lost "after!"]"#);
}
