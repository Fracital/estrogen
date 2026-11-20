# @sysce/crack-linux-arm64

The crack binary for `linux-arm64` (`os: linux`, `cpu: arm64`). Installed as an optional
dependency of [`@sysce/crack`](https://www.npmjs.com/package/@sysce/crack); you
do not install it directly. The launcher picks the package that matches
`process.platform` and `process.arch` and runs `bin/crack`.

CI drops the built binary into `bin/crack` before publishing. In a git
checkout `bin/` holds only this note and a `.gitkeep`.

crack rewrites a built directory in place. It raises the cost of reading that
build. It is not secrecy: a decoder key ships in each file.
