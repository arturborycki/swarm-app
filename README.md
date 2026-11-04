# swarm-app

The source of the **Swarm** app: a macOS terminal whose tabs are persistent
sessions owned by a background daemon, on your Mac or on servers over SSH.
Download and docs: [swarm-term.com](https://swarm-term.com).

This repository is a fork of [Warp](https://github.com/warpdotdev/warp)'s
open-source terminal. Swarm reuses Warp's terminal model, blocks, input editor
and renderer, and adds its own session UI, branding and the connection to its
daemon. It is an independent project, not made by or affiliated with Warp; it
needs no Warp account and hides Warp's cloud services. Warp's original README
is kept as [README.warp.md](README.warp.md).

- **Swarm's changes** are on the `swarm` branch. Most of them live in
  `app/src/swarm/` and are marked `swarm-term:` in comments elsewhere, so the
  diff against upstream stays easy to follow.
- **The daemon, `swarmctl` and the release scripts** are in
  [arturborycki/swarm-term](https://github.com/arturborycki/swarm-term). This
  repository builds inside that one, checked out as its `app/` folder, because
  it uses `crates/swarm-client` from there. See its README, under "Build the
  app".

## License

As in Warp's repository, the UI framework (the `warpui_core` and `warpui`
crates) is licensed under [MIT](LICENSE-MIT) and everything else under
[AGPL-3.0](LICENSE-AGPL). The swarm-term crates it uses are MIT or Apache-2.0.

Report problems at [swarm-term.com/report](https://swarm-term.com/report/) or
in [swarm-term's issues](https://github.com/arturborycki/swarm-term/issues).
