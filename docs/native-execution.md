# Native execution boundaries

The Rust library supplies Linux controlling terminals, byte replay, process
supervision, filesystem/syscall confinement, and owned cgroup v2 limits. The
command-service transport is being integrated; these library primitives alone
are not a complete daemon or a supervisor-crash recovery mechanism.

## Terminal and output ownership

`pty::Terminal` uses a nonblocking PTY master and a single Tokio reactor. The
slave is opened through `TIOCGPTPEER`, then attached as the child's controlling
terminal with `setsid` and `TIOCSCTTY`. Resize and foreground interruption use
kernel terminal operations. Writes report the accepted prefix so a timeout never
requires guessing whether input was delivered. Reads preserve bytes, including
split UTF-8 and arbitrary binary output.

`terminal_buffer::ReplayBuffer` allocates once, retains a fixed tail, and reports
absolute offsets and explicit eviction to independent clients. Capacity is at
most 4 MiB; individual reads are at most 64 KiB. The buffer is not a durable log.
`process_capture` provides bounded asynchronous file capture and reports discarded
bytes. `process_group` holds an unreaped leader until group cleanup, preventing
PID reuse from redirecting a later group signal.

`terminal_session` adds a persistent actor with a 16-request queue and separately
bounded replay retention after command completion. Input sequences start at one.
The same sequence and payload return the original accepted prefix; another
payload under that sequence is rejected. Partial writes and deadlines are
reported explicitly, and the last 64 results remain available. Older retries
fail instead of redelivering input. Accepted operations continue to their original
deadline if a client disconnects. A client sends only an explicitly chosen
remaining suffix under the next sequence. A retained receipt records terminal
status, log counts, cgroup settings and cleanup errors.

## Filesystem and syscall policy

`sandbox::Policy` is operator configuration. Model arguments cannot add read
capabilities, change the scratch directory, or widen the syscall policy.
Preparation requires Landlock ABI 6, both process-domain scopes, the complete
filesystem access set through that ABI, and a compatible seccomp notification
ABI. All requested paths must exist. There is no unconfined fallback.

Read capabilities bind opened inodes. The only general writable tree is an
initially empty, owned, mode-0700 scratch directory. Existing contents are refused
because pre-existing hardlinks could alias outside files. `/dev/null` has fixed
read/write permission; `/dev/urandom` has read permission. No arbitrary devices
are granted. Environment inheritance is cleared; HOME, TMPDIR and XDG_CACHE_HOME
point at scratch. Explicit additional environment entries are operator supplied.
All descriptors above stderr are marked close-on-exec, including accidentally
inherited descriptors lacking that flag. Standard streams must be deliberately
chosen by the caller.

The syscall filter rejects network socket creation and connections, including
host Unix sockets. Anonymous AF_UNIX socketpairs remain available for internal
IPC. It rejects namespace changes, mount APIs, SysV/POSIX named IPC, io_uring,
kernel-management interfaces, external resource changes, and alternate syscall
ABIs. `clone3` returns ENOSYS for the inspectable legacy clone fallback. Only
reviewed terminal ioctls are accepted. Generic syscalls newer than fchmodat2
(number 452 on the supported x86_64/aarch64 ABIs) require an explicit policy
update; they currently return ENOSYS. GPU device ioctls are not enabled by this
backend. Unprivileged threads and subprocesses retain confinement.

Landlock does not mediate every metadata operation. Chown, timestamp and xattr
changes remain denied. Permission changes use a native async broker so linkers
can produce executable files. The broker copies pathname operands into bounded
storage once, resolves them beneath a pinned scratch directory, and operates on
an opened descriptor. It never resumes the intercepted syscall. Descriptor-form
calls are pinned and checked against the same scratch capability. The target
must be owned, must be a regular file or directory, and regular files must have
one link. Set-ID/sticky bits, symlinks, and permission changes to the scratch root
are refused. Notification identities are checked around target lookup. Installing
a replacement seccomp listener is denied. Broker failures propagate to the
process owner.

This backend restricts file access and mutation, not visibility of all filesystem
metadata through `stat` or `readlink`. Explicit read capabilities may include
sensitive files; the operator must choose them accordingly. It shares the host
kernel and does not promise isolation from kernel vulnerabilities or a hostile
unconfined process running as the same UID. The metadata broker currently needs
parent access to the child's `/proc` descriptors and memory; incompatible host
ptrace/proc policy causes operations to fail.

## Resource and descendant containment

`cgroup::Cgroup` creates a unique child beneath an explicitly delegated, UID-owned
cgroup v2 parent with CPU, memory and PID controllers already enabled. It never
enables a parent controller or changes parent limits. CPU quota, memory ceiling,
zero swap, OOM group handling and process count are written and read back before
launch. A preopened membership descriptor moves the child during trusted pre-exec
setup, before payload code can fork. Pair it with filesystem/syscall confinement
so payloads cannot migrate themselves through cgroup files or host service APIs.

Cleanup writes `cgroup.kill`, waits on `cgroup.events` priority notifications with
an explicit deadline, and removes only the directory this instance created.
Detached descendants are included. Call cgroup cleanup before process-group
reaping and broker teardown. Drop attempts emergency cleanup and reports failures;
it cannot provide an asynchronous completion guarantee. The persistent harness
adds an independently owned systemd service so abrupt supervisor death also
triggers descendant cleanup. See [the harness lifecycle](native-harness.md).

Resource caps are per owned cgroup. Admission must also respect the caller's
observed limits and any aggregate budget across sessions. Creating a sibling
cgroup does not automatically preserve an aggregate limit on the caller's former
leaf group. No GPU memory isolation is claimed.

## Validation

Tests ran on Linux 7.0.0-38-generic with Landlock ABI 8 and Rust 1.98.0. Ordinary
PTY/replay/sandbox tests passed; the sandbox cases include C compilation and
execution, source/secret protection, network denials, signal-domain checks,
alternate-ABI rejection, permission-broker races, and terminal job control.
Four delegated-cgroup tests were explicitly enabled with
`FORGE_TEST_CGROUP_PARENT`: CPU throttling, bounded 64 MiB OOM enforcement,
process-count enforcement, and detached-descendant cleanup. They are ignored by
default because they require an operator-selected delegation. No namespace,
AppArmor or other host policy was modified. These are targeted behavioral tests,
not a formal security proof or the complete repository delivery gate.

The kernel contracts are documented in [Landlock](https://docs.kernel.org/userspace-api/landlock.html),
[seccomp notification](https://man7.org/linux/man-pages/man2/seccomp_unotify.2.html),
and [cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html).
