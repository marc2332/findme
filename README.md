# findme

A small CLI that uses TypeSafe's Jev model to search local directory trees from a natural language memory of a file or folder.

## Installation

Install the CLI directly from GitHub:

```sh
cargo install --git https://github.com/marc2332/findme
```

## Usage

Set the API key in the environment:

```sh
export TYPESAFE_API_KEY=your_key
findme "the freya worktree I was working on about improving the font docs"
```

The search starts in the current directory. Use `--root` to choose another starting point:

```sh
findme \
  --root ~/Projects \
  --max-depth 10 \
  --beam-width 12 \
  "the freya worktree about improving the font docs"
```

At each level, `findme` lists entries, applies the repository's `.gitignore` rules, skips `.git`, `.cache`, `.local`, and `.cargo` directories, sends names and lightweight local metadata to Jev, keeps the most promising directories, and continues down those branches. It prints the highest-scoring files and folders it inspected. Symlinks are skipped to avoid leaving the selected search tree.

Useful options:

- `--root PATH` sets the starting directory
- `--max-depth N` limits how far the search descends
- `--beam-width N` controls how many likely directories are explored after the initial parent fallback
- `--results N` controls the number of printed results
- Hidden files and directories are included by default
- `--no-hidden` disables searching hidden files and directories
- `--follow` prints each folder and entry as it is inspected
- Results are filtered relative to the best match by default
- `--all-results` shows every final Jev-ranked result
- Parent fallback is enabled by default and searches up to four parent directories
- `--no-parent-fallback` disables searching parent directories

The API key is never written to disk or included in requests except as the bearer token used by `sysone`.
