# Preset Configurations

Ready-made config files for common project types.

## Usage

Copy the relevant config to your project root and adjust `watch_dir` if needed:

```
cp configs/nextjs.toml /path/to/your/project/replayfs.toml
cd /path/to/your/project
replayfs start --config replayfs.toml --foreground
```

By default, `watch_dir = "."` watches the current working directory. Logs and snapshots go to `.replayfs/` within the watched directory.

## Available presets

- **nextjs.toml** — Next.js apps. Ignores `.next`, `node_modules`, `.turbo`, `.vercel`, and common editor swap files.
