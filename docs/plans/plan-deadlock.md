# Deadlock detection: fibers nothing can wake

**Built 2026-11-19** (runtime.md §4.6).

Decided in runtime.md §13, question 4: when the collector finds a parked
fiber nothing can reach, it posts a deadlock event (as Go does) and the
fiber, instead of being freed, gets a `Deadlock` condition, so it lands in
the debugger's list with its stack (or, with no debugger, fails saying so).
In todo.md since 2026-11-18, when it moved out of M11.

## What "nothing can wake it" means

A parked fiber is woken by whoever holds its waiter: a channel's queue, a
fiber it awaits (its joiners), a scope it joins, a promise, a timer, the
reactor, the blocking pool, a native thread (signals, file watchers, FFI
callbacks, extensions). Every one of those already keeps the waiter, and so
the fiber, reachable: the heap's (channels, fibers, scopes, promises) by
holding it, the runtime's (timers, the reactor's waiters, `sched::root` for
the pool, the main lane, signal and watcher channels, callbacks) as GC
roots, or they'd be waking freed memory. So a parked fiber the collector
finds unreachable can't be woken by anything: it is deadlocked. Today it is
freed without a word.

Two things keep parked fibers reachable that can't wake them:
- **Root tasks** (`-e`, a REPL or editor evaluation, `load`): the scheduler
  roots each so Rust can be told its result. While one is parked that root
  stops counting; the waker's reference is what keeps it.
- **The debugger's list** holds the fibers parked in it, on purpose: they
  wait for a person, and aren't deadlocked.

Not caught (as Go's isn't either): a fiber something reachable could wake
but never will (a channel a live fiber holds and never writes). That's a
liveness bug, not a reachability one.

## How

1. **A registry of fibers** (skein-rt `deadlock.rs`), not traced, as weak
   references are: each fiber goes in when made (`fiber_new`, and fibers an
   image brings back); entries for fibers that ended go at each collection.
2. **At a collection's weak pass** (`process_weak_refs`), before weak
   references are cleared: each registered fiber that's parked and wasn't
   reached is traced (kept, with all it reaches: the trace runs again) and
   put on a ready list, a root until handled. Only a full collection can
   tell: in a young one, fibers (which never move, so are in the non-moving
   space) count as reached.
3. **After it, at a carrier** (`process_external`, where cleanups are
   spawned): each fiber on the list has its waiter claimed (so nothing
   else delivers to it after all), a `{:event :deadlock :fiber id}` posted,
   and is woken with a `Deadlock` error at the operation it parked in: a
   condition like any, which its handlers can take, the debugger keeps
   (`*debugger* :park`), or which ends it.
4. **An idle check.** A program whose only fiber is stuck allocates
   nothing, so no collection would ever look. When every carrier has gone
   idle after fibers ran, with no timer pending, no reactor waiter and no
   blocking job (so nothing outside could still wake a fiber), the last
   carrier to go idle waits a second and, if nothing came, has a full
   collection. A server waiting on its sockets never does this.
   `SKEIN_GC_IDLE=0` turns it off, as it does the idle collection.

`skein -e '(take! (chan))'` then ends with `Deadlock` instead of hanging,
and at the REPL the same expression lands in the debugger.

## Done when

- A detached fiber parked on a channel nothing else holds gets `Deadlock`
  (handled in the fiber; in the debugger's list under `:park`), after a
  full collection; two fibers each waiting on the other both do.
- A fiber parked on what something live holds (a channel, a timer, a
  socket, the debugger) never does, through many full collections.
- `skein -e '(take! (chan))'` ends with the condition, and the idle check
  runs no collection for a process waiting on a socket.
- Every suite passes at both tiers, under GC stress and with everything
  moved at each collection; `spawn and await 10,000` within noise.
