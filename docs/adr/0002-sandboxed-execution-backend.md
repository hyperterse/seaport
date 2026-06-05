# ADR 0002: Sandboxed Execution Backend

## Status

Accepted

## Context

Seaport must run task directories that contain ordinary Linux tooling:
Dockerfiles, shell scripts, verifier scripts, and future agent processes. These
tasks are not Rust programs, and users should not need to rewrite them in Rust or
WebAssembly.

The execution backend therefore needs to provide:

- process isolation
- filesystem isolation
- network policy
- CPU, memory, PID, and wall-clock limits
- read-only task inputs with explicit writable output directories
- compatibility with standard task Dockerfiles and shell scripts

## Research

Docker already provides the compatibility layer Seaport needs. Docker's own
security documentation describes the daemon attack surface and recommends
treating Docker daemon access as privileged; it also documents rootless mode as a
way to run the daemon and containers as a non-root user. Docker's seccomp
documentation says the default profile is an allowlist that blocks many syscalls
while keeping broad compatibility. Docker's `run` reference documents the
runtime flags Seaport needs, including `--network`, `--pids-limit`,
`--read-only`, `--security-opt`, `--tmpfs`, `--mount`, `--memory`, and `--cpus`.

Rust-native Linux primitives are not sufficient as the primary backend:

- Landlock is useful for unprivileged filesystem and network restrictions on
  Linux, but kernel documentation lists ABI/version limits and explicitly calls
  out special filesystem gaps such as `/proc/<pid>/fd/*`.
- bubblewrap is a strong building block, but its maintainers state that it is not
  a complete ready-made sandbox; the caller must define the security model.
- Wasmtime/WebAssembly has a strong sandbox model, but it only runs Wasm modules.
  It does not run arbitrary existing shell/Python/Linux task environments without
  changing the task format.

Stronger isolation options exist, but they are better treated as future OCI
runtime choices:

- gVisor provides a stronger layer between applications and the host by using a
  userspace application kernel and integrates with Docker through `runsc`.
- Firecracker provides microVM isolation, but requires VM/rootfs orchestration and
  is heavier than the first Seaport execution milestone.

Sources:

- Docker Engine security: <https://docs.docker.com/engine/security/>
- Docker seccomp profiles: <https://docs.docker.com/engine/security/seccomp/>
- Docker rootless mode: <https://docs.docker.com/engine/security/rootless/>
- Docker container run reference: <https://docs.docker.com/reference/cli/docker/container/run>
- Docker resource constraints: <https://docs.docker.com/engine/containers/resource_constraints/>
- Linux Landlock userspace API: <https://www.kernel.org/doc/html/latest/userspace-api/landlock.html>
- bubblewrap README: <https://github.com/containers/bubblewrap/blob/main/README.md>
- Wasmtime security: <https://docs.wasmtime.dev/security.html>
- gVisor overview: <https://gvisor.dev/docs/>
- Firecracker README: <https://github.com/firecracker-microvm/firecracker>

## Decision

Seaport's default execution backend is Docker/OCI.

`seaport run -p <task> -a oracle` uses the Docker backend unless the user
explicitly passes `--backend unsafe-local`. The Docker backend builds
`environment/Dockerfile` when present, then runs the solution and verifier in
separate containers with:

- no network by default
- dropped capabilities
- `no-new-privileges`
- read-only root filesystem
- non-root numeric user
- CPU, memory, swap, PID, and wall-clock limits
- read-only task mount
- writable `/app`, `/logs`, `/tmp`, and `/run` only

`unsafe-local` remains available only as an explicitly named development backend.
It is not a sandbox.

## Consequences

- Seaport can run standard task shell scripts without requiring users to write
  Rust.
- The default task path is sandboxed and compatible with Docker-based local
  development on Linux, macOS, and Windows hosts.
- Docker daemon access is still privileged. Production deployments should prefer
  rootless Docker or an OCI runtime such as `runsc` where available.
- Rust-native sandboxing remains useful for future defense-in-depth, but it is
  not the primary compatibility backend.
