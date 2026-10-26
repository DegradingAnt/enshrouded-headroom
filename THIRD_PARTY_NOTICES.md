# Third-party notices

The release bundles Ultimate ASI Loader v10.0.0 and statically links Rust
packages, including the MinHook C library and HDE disassembler.

## MinHook and HDE

The following block is the complete `minhook/LICENSE.txt` from the published
`minhook` 0.9.0 crate. It includes MinHook's BSD-2-Clause notice and the separate
HDE32 and HDE64 BSD notices, including their original separators.
Source: [minhook 0.9.0 on crates.io](https://crates.io/crates/minhook/0.9.0).
The crate archive is pinned by its checksum in `Cargo.lock`.

```text
MinHook - The Minimalistic API Hooking Library for x64/x86
Copyright (C) 2009-2017 Tsuda Kageyu.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

 1. Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.
 2. Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER
OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

================================================================================
Portions of this software are Copyright (c) 2008-2009, Vyacheslav Patkov.
================================================================================
Hacker Disassembler Engine 32 C
Copyright (c) 2008-2009, Vyacheslav Patkov.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

 1. Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.
 2. Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE REGENTS OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

-------------------------------------------------------------------------------
Hacker Disassembler Engine 64 C
Copyright (c) 2008-2009, Vyacheslav Patkov.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

 1. Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.
 2. Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE REGENTS OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Ultimate ASI Loader

The following block is the complete lowercase `license` file from
[Ultimate ASI Loader v10.0.0, commit 01eb152fc5bd659985b722d95cb4ce72c0a1f65d](https://github.com/ThirteenAG/Ultimate-ASI-Loader/blob/01eb152fc5bd659985b722d95cb4ce72c0a1f65d/license).

```text
MIT License

Copyright (c) 2023 ThirteenAG

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

## Rust packages

This list includes all resolved direct, transitive and build dependencies for
Windows x64 with all features enabled. It excludes the workspace package itself.
Build and proc-macro packages appear even when their code is not linked into the
release. The expressions below are the packages' declared SPDX licences.
`configparser` is used under its MIT option. `deny.toml` permits MIT and excludes
LGPL, so LGPL-only dependencies fail the licence check.

Regenerate the list from the repository root with PowerShell 7:

```powershell
$metadata = cargo metadata --format-version 1 --locked --all-features --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
$resolved = @($metadata.resolve.nodes.id)
$metadata.packages |
    Where-Object { $_.id -in $resolved -and $_.id -notin $metadata.workspace_members } |
    Sort-Object name, version |
    ForEach-Object {
        if (-not $_.license) { throw "Missing licence metadata: $($_.name)" }
        '- {0} {1}: {2}' -f $_.name, $_.version, $_.license
    }
```

- ash 0.38.0+1.3.281: MIT OR Apache-2.0
- autocfg 1.5.1: Apache-2.0 OR MIT
- bitflags 2.13.2: MIT OR Apache-2.0
- cc 1.10.0: MIT OR Apache-2.0
- cfg-if 1.0.5: MIT OR Apache-2.0
- chacha20 0.10.2: MIT OR Apache-2.0
- configparser 3.3.0: MIT OR LGPL-3.0-or-later
- core_detect 1.0.0: MIT/Apache-2.0
- cpufeatures 0.3.1: MIT OR Apache-2.0
- fern 0.7.1: MIT
- find-msvc-tools 0.1.15: MIT OR Apache-2.0
- getrandom 0.4.3: MIT OR Apache-2.0
- log 0.4.34: MIT OR Apache-2.0
- minhook 0.9.0: MIT
- num-traits 0.2.19: MIT OR Apache-2.0
- once_cell 1.21.4: MIT OR Apache-2.0
- pin-project-lite 0.2.17: Apache-2.0 OR MIT
- proc-macro2 1.0.107: MIT OR Apache-2.0
- proptest 1.12.0: MIT OR Apache-2.0
- quote 1.0.47: MIT OR Apache-2.0
- rand 0.10.3: MIT OR Apache-2.0
- rand_core 0.10.1: MIT OR Apache-2.0
- rand_xorshift 0.5.0: MIT OR Apache-2.0
- regex-syntax 0.8.11: MIT OR Apache-2.0
- shlex 2.0.1: MIT OR Apache-2.0
- syn 2.0.119: MIT OR Apache-2.0
- tracing 0.1.44: MIT
- tracing-attributes 0.1.31: MIT
- tracing-core 0.1.36: MIT
- unarray 0.1.4: MIT OR Apache-2.0
- unicode-ident 1.0.27: (MIT OR Apache-2.0) AND Unicode-3.0
- windows-link 0.2.1: MIT OR Apache-2.0
- windows-sys 0.61.2: MIT OR Apache-2.0
