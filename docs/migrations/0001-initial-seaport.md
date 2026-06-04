# Migration 0001: Initial Seaport Adoption

## Applies To

Projects adopting Seaport `0.1.0`.

## Steps

1. Install the `seaport` CLI with `cargo install --path .`.
2. Create task skeletons with `seaport init --task <org/name>`.
3. Fill in `instruction.md`, `task.toml`, `environment/Dockerfile`, and
   `tests/test.sh`.
4. Use the planned `seaport run -p <path> -a <agent> -m <model>` command shape
   for local task execution.
5. Keep Rust library integration limited to internal engine development unless a
   downstream tool explicitly needs it.

## Rollback

Remove generated task directories and uninstall the local CLI with
`cargo uninstall seaport`. No data migration is required because Seaport `0.1.0`
does not persist external state outside generated task or job directories.
