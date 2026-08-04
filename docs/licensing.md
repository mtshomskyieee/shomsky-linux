# Licensing

Project policy for MIT, AS-IS / use-at-your-own-risk, and AI-assisted
contributions is decided in
[ADR-0006](adr/0006-mit-as-is-and-ai-contributions.md). This page is the
SPDX / inventory detail.

## Shomsky Linux original code

All original Shomsky Linux source in this repository (engram-shell, scripts,
docs, skills, container glue) is licensed under the **MIT License**. See
[LICENSE](../LICENSE). There is **no warranty**; operators run the software
and container **at their own risk** (ADR-0006).

## Alpine Linux base and official Docker image

Shomsky Linux is **based on Alpine Linux** and the container is built
`FROM alpine:<version>` — the [official Alpine Linux Docker image](https://hub.docker.com/_/alpine)
maintained in [alpinelinux/docker-alpine](https://github.com/alpinelinux/docker-alpine).

### docker-alpine (MIT)

[docker-alpine](https://github.com/alpinelinux/docker-alpine) is **MIT**-licensed:

> Copyright (c) 2019 Natanael Copa

MIT requires that the copyright notice and permission notice be included in
all copies or substantial portions of the Software. We retain a verbatim copy
at [third_party/docker-alpine/LICENSE](../third_party/docker-alpine/LICENSE)
and record the attribution in [NOTICE](../NOTICE) and
[third_party/docker-alpine/NOTICE](../third_party/docker-alpine/NOTICE).

### Alpine package aggregate

Alpine is not a single-licensed project; the container image aggregates
packages under many licenses, notably:

| Component | Typical license |
|-----------|-----------------|
| musl libc | MIT |
| BusyBox | GPL-2.0 |
| apk-tools | GPL-2.0 |
| alpine-baselayout | GPL-2.0 |

We redistribute **unmodified** Alpine packages as part of the OCI image. That
is ordinary binary redistribution. GPL obligations for those components are
satisfied by:

1. Shipping them unmodified
2. Pointing recipients at Alpine’s published sources
   ([git.alpinelinux.org](https://git.alpinelinux.org/),
   [pkgs.alpinelinux.org](https://pkgs.alpinelinux.org/))
3. Recording this in [NOTICE](../NOTICE)

### Trademark

“Alpine” and the Alpine Linux name/logo are trademarks of their respective
owners. Shomsky Linux is an independent project and is **not affiliated with,
endorsed by, or sponsored by** the Alpine Linux project. Brand as
“Shomsky Linux, based on Alpine Linux”; do not use Alpine’s marks as our own.
Do not use the Alpine logo without permission from the Alpine Linux project.

## Inference stack

| Component | License |
|-----------|---------|
| mistral.rs | MIT |
| Candle (Hugging Face) | Apache-2.0 |
| engram-shell dependencies | see each crate (MIT / Apache-2.0 preferred) |

## Model weights

GGUF weights are **never redistributed** in the image or git repository. They
are downloaded by the end user from Hugging Face Hub under each model’s own
license (typically Apache-2.0 for the default Mistral / Qwen aliases; see
`models.toml`).

## SPDX summary for the image

```
Shomsky original code: MIT
docker-alpine base:    MIT (Copyright (c) 2019 Natanael Copa)
mistral.rs:            MIT
Candle:                Apache-2.0
Alpine aggregate:      various, including GPL-2.0 (BusyBox, apk-tools, …)
Model weights:         not included; end-user download
```
