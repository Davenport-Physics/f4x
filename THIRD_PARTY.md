# Third-party sources

## CommonLibF4

f4x derives engine type layouts, address IDs, and the F4SE-compatible plugin ABI
from CommonLibF4 (https://github.com/Ryan-rsm-McKenzie/CommonLibF4), used under
the MIT license. Facts taken from it are tagged `clf4` in `provenance/` with the
commit and file:line they came from.

Reference commits: `master` 67173f8d44b9a928dbab983d25e576ac5f1e83ac,
`origin/community` da3e99588ac9b688ed8474736b6bbc0e0ead4f4c.

```
MIT License

Copyright (c) 2019 ryan-rsm-mckenzie

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## CommonLibF4 next-gen fork (libxse/commonlibf4)

f4x's next-gen (1.11.x) layout tables (`crates/f4x-engine-sys/src/ng_1_11_240.rs`) are
generated from the headers of libxse/commonlibf4 (https://github.com/libxse/commonlibf4),
the next-gen fork of CommonLibF4, at commit 40a93f6917267f30d3551334ccd280402941c3c2
(2026-07-26), its last commit under the MIT license (same license text as above,
Copyright (c) 2019 ryan-rsm-mckenzie). Later commits are GPL-3 and are not used. Facts
from it are tagged `clf4` with `"repo":"libxse/commonlibf4","commit":"40a93f69"` in
`provenance/`.

The fork's `lib/commonlib-shared` submodule (REL/REX) is GPL and is not used: f4x's own
MIT shims in `crates/f4x-codegen/shim-ng/` stand in for it when the headers are parsed
on the host. Nothing from either ships in f4x.
