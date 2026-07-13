# Third-Party Notices

Open Chronicle Next is licensed under the MIT License. The following notices cover
source material and direct Rust dependencies present in the U1a workspace. A
complete transitive software-bill-of-materials and license inventory is a release
gate and will be generated from the locked dependency graph before distribution.

## Screenata/open-chronicle

Product interaction concepts were reviewed from `Screenata/open-chronicle` at
commit `80437271e509c6dd2eba7be7c216e21c76aa41c5` under the MIT License. No runtime
source code is copied in U1a.

> MIT License
>
> Copyright (c) 2026 open-chronicle contributors
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of
> this software and associated documentation files (the "Software"), to deal in
> the Software without restriction, including without limitation the rights to
> use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
> the Software, and to permit persons to whom the Software is furnished to do so,
> subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
> FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
> COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
> IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
> WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Rust dependencies

The locked graph is authoritative. Direct dependencies introduced by the workspace
scaffold are:

| Package | Declared version | License |
| --- | --- | --- |
| `chrono` | 0.4.45 | MIT OR Apache-2.0 |
| `rusqlite` | 0.40.1 | MIT |
| `serde` | 1.0.228 | MIT OR Apache-2.0 |
| `serde_json` | 1.0.150 | MIT OR Apache-2.0 |
| `sha2` | 0.11.0 | MIT OR Apache-2.0 |
| `thiserror` | 2.0.18 | MIT OR Apache-2.0 |
| `uuid` | 1.23.5 | Apache-2.0 OR MIT |

The `bundled` `rusqlite` feature compiles SQLite into Chronicle. SQLite is in the
public domain; see <https://www.sqlite.org/copyright.html>.

