# rbind — `LD_PRELOAD` shim that rewrites network syscalls

Produces a `cdylib` that intercepts 11 glibc symbols via `LD_PRELOAD` and rewrites their behavior according to short environment variables (`BIND_DEVICE`, `BW`, `ADDRESS_V4`, …) — without recompiling or relinking the host program.

The shim is loaded once at process start, applies its configuration lazily on the first network syscall, and runs entirely in userspace via `dlsym(RTLD_NEXT, …)`.

The repo ships two artifacts:

- **`rbind`** — a CLI driver (`src/cli/`) that parses flags, locates the shim, sets `LD_PRELOAD`, and `exec()`s the target.
- **`librbind_lib.so`** — the shim itself (`src/shim/`), the `cdylib` that gets `LD_PRELOAD`-ed into the host.

---

## Quick start

```bash
# 1. Build the .so and drop it next to the rbind binary in one step:
cargo build --release && ./target/release/rbind generate
# (or just `rbind generate` if rbind is already on $PATH and you ran
#  `cargo build --release` yourself — the file lands at target/<profile>/)

# 2. Optional: pin a default device or throttle once via a profile.
mkdir -p ~/.config/rbind
cat > ~/.config/rbind/profiles.toml <<'EOF'
[vpn]
BIND_DEVICE = "wg0"

[throttle-slow]
BW = 65536
EOF

# 3. Run it. The CLI exec()s the target, so signals and exit codes pass through:
./target/release/rbind run -p vpn --address-v4 10.0.0.5 -- ./my_app
./target/release/rbind show -p throttle-slow --bw 1mb     # see the env-var set
./target/release/rbind profile list                       # list known profiles
```

If you can't or don't want to use the CLI, see [Direct `LD_PRELOAD` usage](#direct-ld_preload-usage) below.

---

## CLI reference

`rbind <SUBCOMMAND> [args]`

| Subcommand | Purpose |
|---|---|
| `rbind generate` | Build the cdylib and place it next to the `rbind` binary so the next `rbind run` finds it without `RBIND_SO_PATH` or `config.toml`. |
| `rbind run [-p profile] [flags] -- <cmd> [args…]` | Build the env, locate the `.so`, set `LD_PRELOAD`, and `exec()` the target. |
| `rbind show [-p profile] [flags]` | Print `LD_PRELOAD=…` and the merged env-var set without executing anything. |
| `rbind profile list [--name NAME]` | List profile names from `~/.config/rbind/profiles.toml` (or dump one profile's env vars with `--name`). |

### `rbind generate`

A no-arg convenience that:

1. Runs `cargo build --release` (must be invoked from the rbind project root, since it shells out to `cargo`).
2. Copies `target/release/librbind_lib.so` into the same directory as the running `rbind` binary (using `current_exe().parent()`).

That second step matters: the launcher's directory-walk step in [`find_so`](#how-rbind-finds-the-so) immediately finds the `.so` next to itself, so subsequent `rbind run` invocations need no env var, no config file, and no system install.

### `rbind run` flags

Every flag writes the short env-var name (same as the flag, no `RBIND_` / `FORCE_NET_` prefix). Flag values override profile values for the same key.

| Flag | Env var | Notes |
|---|---|---|
| `--address-v4 <IP>` | `ADDRESS_V4` | Use `deny` to make every `bind(2)` return `EACCES`; `fake` to make it return `0` without the syscall. |
| `--address-v6 <IP>` | `ADDRESS_V6` | |
| `--port-v4 <N>` | `PORT_V4` | Host order. `0` = "pick an ephemeral port" (just unset it). |
| `--port-v6 <N>` | `PORT_V6` | |
| `--bind-device <name>` | `BIND_DEVICE` | Needs `CAP_NET_RAW` / root. |
| `--tos <hex>` | `TOS` | |
| `--ttl <N>` | `TTL` | |
| `--ka <sec>` | `KA` | `0` disables keepalive. |
| `--mss <N>` | `MSS` | |
| `--fwmark <hex>` | `FWMARK` | |
| `--prio <N>` | `PRIO` | 0–6. |
| `--reuseaddr` | `REUSEADDR=1` | Boolean flag. |
| `--nodelay` | `NODELAY=1` | Boolean flag. |
| `--bw <N[kb\|mb]>` | `BW` | Accepts `65536`, `1kb`, `1mb`, etc. |
| `--bw-per-socket <N>` | `BW_PER_SOCKET` | Mutually exclusive with `--bw`. |
| `--poll-timeout <ms>` | `POLL_TIMEOUT` | |
| `--verbose <N>` | `VERBOSE` | 0=errors, 1=every hook, 2=also throttle timing. |
| `--log <path>` | `LOG` | |

### How `rbind` finds the `.so`

The launcher walks this list, first match wins:

1. **`$RBIND_SO_PATH`** — explicit per-invocation override. Wins over everything else.
2. **`so_path`** from `~/.config/rbind/config.toml` — persistent user-level setting. See [Config](#config).
3. **Next to the `rbind` binary** — `./librbind_lib.so`, `./librbind.so`, `./rbind.so`. (`rbind generate` populates this slot.)
4. **System install paths** — `/usr/local/lib/librbind_lib.so`, `/usr/lib/librbind_lib.so`, and the legacy `librbind.so` variants.

Both `$RBIND_SO_PATH` and `config.toml` `so_path` accept a leading `~` for the current user's home directory. `~user/...` is **not** expanded (no portable NSS lookup on Linux).

---

## Config

Two user-level files, both in `~/.config/rbind/`. Both are read **lazily**; a missing file is not an error.

### `config.toml` — install location

```toml
# ~/.config/rbind/config.toml
so_path = "~/.cargo/bin/librbind_lib.so"   # ~ is expanded to $HOME
```

Currently a single field. Future keys (default profile, log dir, …) will land at the same top level.

### `profiles.toml` — reusable env-var bundles

```toml
# ~/.config/rbind/profiles.toml
[vpn]
BIND_DEVICE = "wg0"

[throttle-slow]
BW = 65536
```

Each top-level table is a named profile; the keys inside are the short env-var names the shim reads (same as the CLI flag names — see [The shim (lib) — what env vars do](#the-shim-lib--what-env-vars-do)). Profile values are merged first, then CLI flags override on a per-key basis.

---

## The shim (lib) — what env vars do

The shim has no flags of its own; everything is configured through environment variables that the host process inherits from `rbind run`'s merged env. The env-var names match the short forms used by the CLI flag names (e.g. `--bind-device` → `BIND_DEVICE`), so profile entries in `~/.config/rbind/profiles.toml` use the same key as the flag.

| Env var | Effect |
|---|---|
| `ADDRESS_V4` | Overwrite the IPv4 source address passed to `bind(2)`/`connect(2)`/`sendto(2)`. Special values: `deny` → `EACCES`; `fake` → return `0` without the syscall. |
| `ADDRESS_V6` | Same, for IPv6. |
| `PORT_V4` / `PORT_V6` | Overwrite the v4 / v6 port (host order). |
| `TOS=0xNN` | IP TOS / DSCP field (`setsockopt(IP_TOS)`). |
| `TTL=N` | IPv4 TTL (`setsockopt(IP_TTL)`). |
| `KA=N` | TCP keepalive idle time (sec); `0` disables. Triggers `SO_KEEPALIVE` + `TCP_KEEPIDLE`. |
| `MSS=N` | TCP max segment size (`TCP_MAXSEG`). |
| `REUSEADDR=1` | Force `SO_REUSEADDR`. |
| `NODELAY=1` | Disable Nagle (`TCP_NODELAY`). |
| `FWMARK=0xN` | Netfilter packet mark (`SO_MARK`); root required. |
| `PRIO=N` | `prio` qdisc band 0–6 (`SO_PRIORITY`). |
| `FLOWINFO=0xCCCCLLLLL` | IPv6 traffic class + flowlabel. Sets `sin6_flowinfo` + `IPV6_FLOWLABEL_MGR` + `IPV6_FLOWINFO_SEND`. |
| `BIND_DEVICE=eth0` | Bind every socket to a specific interface (`SO_BINDTODEVICE`); `CAP_NET_RAW` required. |
| `BW=N` | Cap **all** sockets to N bytes/sec (token bucket). |
| `BW_PER_SOCKET=N` | Cap each socket independently. **Mutually exclusive** with the global. |
| `POLL_TIMEOUT=N` | Override every `poll(2)` `timeout` argument. |
| `LOG=path` | Write a per-line trace of every intercepted syscall. |
| `VERBOSE=N` | `0` (default, errors only), `1` (every hook), `2` (also bandwidth-sleep timing). |

With **no** env vars set, the shim is a no-op — it forwards every call to libc unchanged.

---

## Direct `LD_PRELOAD` usage

If the CLI is in the way (cross-compiled binary, no shell, custom launcher, …) you can skip it entirely:

```bash
# Symlink to the un-prefixed name (optional; LD_PRELOAD will find either):
ln -s "$(pwd)/target/release/librbind_lib.so" ./rbind.so

# Use on any binary:
LD_PRELOAD=./rbind.so \
  ADDRESS_V4=10.0.0.5 \
  PORT_V4=9000 \
  ./your_program
```

In this mode the env vars in [The shim (lib) — what env vars do](#the-shim-lib--what-env-vars-do) are the entire interface; the shim does not consult `rbind` CLI flags.

---

## Real-world recipes

Each recipe is shown in CLI form; the equivalent raw `LD_PRELOAD` invocation is in the [Direct `LD_PRELOAD` usage](#direct-ld_preload-usage) section above.

### Force a client to use a specific source IP

```bash
rbind run --address-v4 10.0.0.5 -- ./my_app
```

### Make a server bind to localhost only

```bash
rbind run --address-v4 127.0.0.1 -- ./daemon --port 8080
```

### Throttle an uploader

```bash
rbind run --bw 1mb -- ./scp bigfile.tar.gz remote:/data/
```

### Stamp every packet with DSCP `EF` (46 = `0xb8`) for QoS testing

```bash
rbind run --tos 0xb8 -- ./media_streamer
```

### Tag packets for netfilter routing (`FWMARK`)

```bash
sudo rbind run --fwmark 0x42 -- ./my_app
# combine with iptables -t mangle … to route the app's traffic through a separate WAN
```

### Pin a process to a specific network interface

```bash
# Useful for VPN tunnels (e.g. WireGuard), forcing a backup link,
# or sandboxing into a netns. Requires CAP_NET_RAW; the hook silently
# swallows EPERM if not granted.
sudo rbind run --bind-device wg0 -- ./my_app
```

### Isolate Skype-style `poll(…, timeout=0)` busy-loops

```bash
rbind run --poll-timeout 2000 -- ./chatty_app
```

### Verify forced bind with strace

```bash
rbind run --address-v4 10.0.0.5 -- strace -e trace=bind,connect,sendto -f ./app
```

You'll see the **rewritten** sockaddr in each syscall — the program's source code is irrelevant.

### Debug what the shim is doing

```bash
rbind run --verbose 2 --log /tmp/rb.log -- ./app
# then in another terminal:
tail -f /tmp/rb.log
```

Verbose levels: `1` = one line per hook call; `2` = also `bw()` timing and `change_local_binding` traces.

---

## Architecture

```
┌───────────────────────────────────────┐
│  host process (any)                   │
│  ┌────────────────────────────────┐   │
│  │ socket(2) → bind(2) → connect  │   │ ← libc socket layer
│  └────────────┬───────────────────┘   │
│               ↓ PLT                   │
│  ┌────────────┴────────────────────┐  │
│  │  rbind (librbind_lib.so)        │  │ ← LD_PRELOAD
│  │  ┌─ syscalls::resolve_all() ──┐ │  │     
│  │  │  dlsym(RTLD_NEXT, "bind")  │ │  │  
│  │  └────────────────────────────┘ │  │ 
│  │  ┌─ xlog! → log::write ──────── │  │ 
│  │  │   AtomicI32 fd             │ │  │ 
│  │  │   libc::write (no Mutex!)  │ │  │ 
│  │  └────────────────────────────┘ │  │ 
│  │  ┌─ hooks (reentrance guard) ─┐ │  │ 
│  │  │  thread_local IN_HOOK      │ │  │ 
│  │  │  if is_reentrant() {       │ │  │ 
│  │  │    call original only      │ │  │ 
│  │  │  }                         │ │  │ 
│  │  └────────────────────────────┘ │  │ 
│  │  ┌─ bw::throttle ─────────────┐ │  │ 
│  │  │   token bucket             │ │  │ 
│  │  │   nanosleep EINTR loop     │ │  │ 
│  │  └────────────────────────────┘ │  │ 
│  │  ┌─ fd_table:─────────────────┐ │  │ 
│  │  │   Mutex<HashMap<RawFd,     │ │  │ 
│  │  │     SocketInfo>>           │ │  │ 
│  │  └────────────────────────────┘ │  │ 
│  └─────────────────────────────────┘  │
└───────────────────────────────────────┘
```

### Module map

The shim lives in `src/shim/`; the CLI driver lives in `src/cli/`. `src/lib.rs` declares both.

| File | Purpose |
|---|---|
| `src/lib.rs` | crate root — `pub mod cli; pub mod shim;` |
| `src/shim/mod.rs` | shim module declarations + `.init_array` rationale |
| `src/shim/init.rs` | `OnceLock<Config>` — lazy init, called from each hook |
| `src/shim/config.rs` | `Config` struct + `load_from_env()` (parses all short env vars: `BIND_DEVICE`, `BW`, `ADDRESS_V4`, …) |
| `src/shim/log.rs` | `AtomicI32` log fd + `xlog!` macro + verbose threshold |
| `src/shim/syscalls.rs` | `Fn*` typedefs + `dlsym(RTLD_NEXT,…)` resolver + `OLD_*` slots |
| `src/shim/fd_table.rs` | `Mutex<HashMap<RawFd, SocketInfo>>` |
| `src/shim/socket.rs` | `SocketInfo` struct + `FB_FLAGS_*` constants |
| `src/shim/sockaddr.rs` | `sockaddr_storage` views + `format()` + `alter_sa()` |
| `src/shim/setsockopt.rs` | intercept table for the 10 known `(level, optname)` pairs (incl. `SO_BINDTODEVICE`) |
| `src/shim/flowinfo.rs` | v6 `IPV6_FLOWLABEL_MGR` + `IPV6_FLOWINFO_SEND` + override `sin6_flowinfo` |
| `src/shim/bw.rs` | token-bucket throttle + `nanosleep` EINTR loop |
| `src/shim/hooks.rs` | all 11 `#[no_mangle] pub unsafe extern "C"` hooks + reentrance guard |
| `src/shim/consts.rs` | local shims for `SO_MARK`, `SOCK_DCCP`, v6 flowinfo constants, `struct in6_flowlabel_req` |
| `src/cli/mod.rs` | clap `Cli` + `Command` enum (`run` / `show` / `profile list` / `generate`) |
| `src/cli/rbind.rs` | thin `fn main()` calling `rbind_lib::cli::run()` |
| `src/cli/flags.rs` | `ForceFlags` struct + `apply_to` flag→env-var mapping + `parse_bandwidth` |
| `src/cli/profile.rs` | TOML profile loader (`~/.config/rbind/profiles.toml`) + `profile_to_env` + `sorted_names` |
| `src/cli/config.rs` | TOML app-config loader (`~/.config/rbind/config.toml`, currently `so_path`) |
| `src/cli/launch.rs` | `find_so()` (env var → config → dir walk → system) + `exec_with_env()` |
| `src/cli/generate.rs` | `rbind generate` — `cargo build --release` + copy `.so` next to the binary |

### Hook lifecycle on a typical call

```
socket(AF_INET, SOCK_STREAM, 0)
  ↓
[hook] socket()
   ├─ shim::init::init()                ← lazy, once per process
   │   ├─ log::open(LOG)                  ← libc::open, atomic fd store
   │   ├─ log::set_verbose(VERBOSE)
   │   ├─ Config::load_from_env()         ← parses every short env var
   │   ├─ shim::bw::set_global_limit(BW)
   │   └─ syscalls::resolve_all()         ← dlsym(RTLD_NEXT,…) every OLD_*
   ├─ xlog!(1, "socket(domain=…)")
   │   └─ log::write()                    ← libc::write(LOG_FD,…) (no Mutex)
   ├─ OLD_SOCKET(domain, type, proto)    ← real socket via fn pointer
   ├─ socket_create_callback(fd,…)
   │   ├─ for each short env var (TOS, TTL, …):
   │   │     old_setsockopt(fd, level, name, &forced, sizeof(int))
   │   └─ fd_table::add(fd, SocketInfo{…})
   └─ return fd
```

---

## Build & install

### Build

```bash
cargo build --release
# → target/release/librbind_lib.so  (the cdylib shim, ~560 KB)
# → target/release/rbind            (the CLI driver, ~1.9 MB)
```

### Make the .so discoverable for `rbind run`

The simplest path: after `cargo build`, run `./target/release/rbind generate`. It re-runs `cargo build --release` and copies `target/release/librbind_lib.so` into the same directory as the `rbind` binary, which is the first place `find_so()` looks.

For a system-wide install (so `rbind` is on `$PATH` for all users):

```bash
install -m 0755 target/release/rbind             /usr/local/bin/rbind
install -m 0755 target/release/librbind_lib.so  /usr/local/lib/rbind.so
```

For a per-user install, either set `RBIND_SO_PATH` in your shell rc, or drop a `config.toml`:

```toml
# ~/.config/rbind/config.toml
so_path = "~/.local/lib/librbind_lib.so"
```

### Run the unit tests (no `LD_PRELOAD` needed)

```bash
cargo test --lib
```

---

## What it can't do (and why)

1. **No `AF_UNIX` interference.** Sockets that aren't `AF_INET` / `AF_INET6` are passed through verbatim — the fd table simply doesn't mark them as network sockets.
2. **No redirect of `recv` / `recvfrom` / `recvmsg`.** Only outbound-rewrite hooks are implemented.
3. **No `setsockopt` for unknown `(level, optname)` pairs.** The intercept table covers the 9 pairs the C version covered; everything else falls through.
4. **No data mutation on send.** The shim throttles and adds metadata; it doesn't rewrite the bytes.
5. **Linux / Android only.** The `libc::in6_flowlabel_req` shim in `consts.rs` and a `compile_error!` elsewhere lock the project to those targets.

---

## Notable differences from the C version

| | C original | Rust rewrite | Why |
|---|---|---|---|
| Logger | `fopen` + `FILE*` + `setlinebuf` | `libc::open` + `AtomicI32` fd | `Mutex<Option<File>>` from inside an `LD_PRELOAD` hook deadlocks against glibc's internal allocator/loader locks (verified empirically — the same hang occurs with raw `libc::open` + `libc::write`, ruling out std::fs as the cause). Atomic primitives are lock-free. |
| Reentrance guard | none (relied on lazy `init()` plus short hook bodies) | `thread_local! IN_HOOK: Cell<bool>` checked at the top of every hook | `xlog!` → `libc::write` → our `write` hook → `bw::throttle` → `fd_table::get` → `Mutex::lock` would otherwise recurse and deadlock. Hooks check `is_reentrant()` and pass through to the original libc function when re-entered. |
| Env-var logging | one log line per var inside `init()` | per-var logging **moved out** of `init()` | `init()` runs lazily from a hook, mid-libc-call. Writing to the log file at that point hit a deeper glibc deadlock that the reentrance guard doesn't fix. The forced actions are still visible in hook log lines (e.g. `bind(sockfd=4, IPv4/127.0.0.2/900)`). |
| `.init_array` constructor | none | none | Adding one would let `init()` run at `.so` load, but `writeln!` (or even raw `libc::write`) to the log file from there hangs on glibc. Lazy-on-first-hook matches the C version. |
| sockaddr byte order | implicit `htons`/`htonl` via `inet_pton` + `sin_port = htons(p)` | explicit `.to_be()` / `from_be()` for every `s_addr` / `sin_port` write/read | Rust's `u32::from_be(x)` makes the kernel's network-order convention explicit; one `u32::ipv4_round_trip` unit test catches byte-order bugs. |
| IPv6 flowinfo | `htonl(flowinfo & IPV6_FLOWLABEL_MASK)` into `mgr.flr_label` | `(forced & IPV6_FLOWLABEL_MASK).to_be()` | Same as above. |
| Bandwidth state | `static struct private bw_global;` + gettimeofday-based `timeval last` | `static BwState { limit, rest, last: Option<Instant> }` | `Instant` is monotonic — `gettimeofday` can jump backward on `ntpdate` and break the throttle. |
| Sleep primitive | `nanosleep` in EINTR loop | `libc::nanosleep` in EINTR loop | `std::thread::sleep` is **not** signal-safe; signal delivery to the host process must interrupt our sleep and let the handler run. `libc::nanosleep` is what an LD_PRELOAD shim needs. |

---

## How reentrance works in detail

LD_PRELOAD shims run inside arbitrary host processes with arbitrary signal handlers. Two failure modes we explicitly avoid:

1. **glibc reentrance.** Calling `Mutex::lock` (→ `pthread_mutex_lock`) from inside an `LD_PRELOAD` hook can deadlock against glibc's internal allocator/loader locks, because the host process is mid-libc-call. We mitigate this by:
   - Using `AtomicI32` for the log fd.
   - Using `LazyLock<Mutex<BwState>>` and `LazyLock<Mutex<HashMap<…>>>` only for state we briefly touch from inside hooks; for the log path we have no Mutex at all.
2. **Self-recursion.** `xlog!` calls `libc::write`, which would hit our `write` hook, which would call `bw::throttle`, which would call `xlog!` (or `fd_table::get` → Mutex lock) — infinite recursion / deadlock.

The fix is the `IN_HOOK` thread-local in `src/shim/hooks.rs`:

```rust
thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}
fn is_reentrant() -> bool { IN_HOOK.with(Cell::get) }
fn enter_hook() -> Option<HookGuard> { /* sets IN_HOOK=true */ }

#[no_mangle]
pub unsafe extern "C" fn write(fd, buf, len) -> ssize_t {
    crate::shim::init::init();
    let fp = OLD_WRITE.get().unwrap();
    if is_reentrant() { return fp(fd, buf, len); }   // ← key line
    let _guard = enter_hook();                         // sets IN_HOOK=true
    let n = fp(fd, buf, len);
    shim::bw::throttle(fd, n.max(0) as usize);        // safe: not re-entrant
    n
}
```

When `xlog!` (inside `bw::throttle` or `init()`) calls `libc::write`, the re-entered hook sees `IN_HOOK=true` and just forwards to the original libc function — no `bw::throttle`, no fd table access, no deadlock.

---

## Architecture invariants worth knowing

- **Lazy init.** `init()` runs once per process, on the first hook call. No `.init_array` constructor. If the host program never calls any of the 11 hijacked symbols, `init()` never runs.
- **No logging from `.init_array`.** Confirmed deadlock empirically on glibc 2.x with both `std::fs::File` and raw `libc::open`/`libc::write`.
- **Atomic-only log state.** The log path has zero `Mutex` instances.
- **All `OLD_*` resolved via `dlsym(RTLD_NEXT, …)`, not direct imports.** Calling the resolved function pointer bypasses the PLT and prevents the host process's `write()` calls from being shadowed by our hook (when we want to fall through). Direct `extern "C" { fn write(...) }` would loop.
- **Hook results are ignored on forced-option setsockopt failures.** Matches the C version's behavior. The `#[must_use]` warnings on `set_*` in `hooks.rs` are silenced with `let _ =`.

---

## License

GPLv3
