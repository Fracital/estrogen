# @sysce/estrogen-darwin-arm64

The estrogen binary for `darwin-arm64` (`os: darwin`, `cpu: arm64`). Installed as an optional
dependency of [`@sysce/estrogen`](https://www.npmjs.com/package/@sysce/estrogen); you
do not install it directly. The launcher picks the package that matches
`process.platform` and `process.arch` and runs `bin/estrogen`.

CI drops the built binary into `bin/estrogen` before publishing. In a git
checkout `bin/` holds only this note and a `.gitkeep`.

estrogen rewrites a built directory in place. It raises the cost of reading that
build. It is not secrecy: a decoder key ships in each file.
