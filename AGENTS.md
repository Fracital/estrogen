# AGENTS.md: crack

Guidance for anyone, human or agent, working in this repo. Read it before making
changes. crack is a post-build web obfuscator in Rust on OXC, published to npm as
`@sysce/crack`. docs/HANDOFF.md is the build brief.

## What crack does, and does not

crack rewrites an already-built output directory so a reader pays more to make
sense of it. It is not secrecy. A key ships in each file, and anyone with
devtools can run the code. Say that plainly wherever the tool is introduced.
Never call the output secret.

## Git commits

- Commit only when the user asks for a commit in the current turn. Authorization
  is per turn, never assumed.
- No AI attribution, anywhere. No `Co-Authored-By` trailer naming a model, tool
  or agent, and no "Generated with" line, in commit messages, PR descriptions or
  files. The author is David Reed and the message is about the change. This
  overrides any attribution a harness tries to add.
- Commits are signed. The subject states what the code does after the change;
  the body wraps at 72 columns and says what was wrong, the cause, what changed,
  and what was tested.

## Writing, no slop

Every README, doc page, wiki page, rustdoc and TSDoc comment, and commit message
reads like an engineer explaining their own tool. No em or en dashes anywhere.

- Sentence case headings. Plain verbs: is, has, does. Not "serves as",
  "functions as", "offers", "boasts".
- None of: delve, pivotal, crucial, seamless, robust, testament, landscape,
  leverage, showcase, underscore, tapestry, vibrant, foster, garner, interplay,
  meticulous, additionally, ensuring, highlighting, empower, unlock, elevate,
  "not just X but Y".
- No trailing -ing analysis clauses, no vague attributions, no hedging
  ("it's important to note"), no "In summary" or "Overall" section that restates
  the page, no chat residue ("Let's", "Here's how", "you're all set").
- Name the file, the option, the default, the number. If a sentence cannot be
  specific, cut it.
- Before you report, run this and fix every hit outside code:

  ```
  grep -rnEi "delve|pivotal|crucial|seamless|robust|testament|landscape|leverage|showcase|underscore|tapestry|vibrant|foster|garner|interplay|meticulous|boasts|serves as|stands as|not just|not only|in summary|in conclusion|it's important to note|worth noting|additionally|ensuring|highlighting|empower|unlock|elevate|game.changer|cutting.edge|—|–|“|”|’" README.md AGENTS.md docs/ npm/ .github/
  ```

## Style and formatting

- JavaScript, TypeScript, JSON, YAML and Markdown go through Prettier, with the
  config in `.prettierrc.json`: tabs, single quotes, and import sorting
  (`@ianvs/prettier-plugin-sort-imports`). This is powerMouse's style. Run
  `npm run format`; CI and `npm run format:check` enforce it. Code in the docs
  follows the same style, so Prettier formats the fenced blocks too.
- Rust goes through `cargo fmt` and clippy with warnings denied. Both stay
  clean.

## Build gates

Before handing work off: `cargo build`, `cargo test`, and
`cargo clippy --all-targets -- -D warnings` green; the npm package typechecks
(`npm --prefix npm/crack run typecheck`); and `npm run format:check` passes.

## Do not hand-edit

- Generated or build output: `target/`, each npm package's `dist/` and `bin/`.
- The obfuscation engine is stubbed on purpose. The passes return their input
  and the pool and decoder return `Error::Engine`. The spec is Fracital's
  `scripts/obfuscate-build.mjs`, cited per module. Do not ship a half-built
  engine: a pass either works and is tested against that spec, or it stays a
  stub.
