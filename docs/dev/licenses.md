# 依存ライセンス一覧 (自動生成)

- 生成日: 2026-08-30
- 生成コマンド: `scripts/gen-licenses.sh` (要 cargo / jq / pnpm)
- 内容: Cargo.lock解決グラフの全Rustクレート + apps/desktopのnpm依存 (prod+dev)。
  依存を追加・更新した際は再生成してコミットすること。
- Rust側のworkspaceメンバー (gento-core/gento-image/gento-tauri) は本リポジトリ管理。
  license欄が空のクレートはpackage.json/Cargo.tomlにlicense宣言がないもの
  (入手元のリポジトリで確認すること)。

## Rust (workspace + Cargo.lock依存)

| クレート | バージョン | license | 入手元 |
|---|---|---|---|
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | https://github.com/oyvindln/adler2 |
| ahash | 0.8.12 | MIT OR Apache-2.0 | https://github.com/tkaitchuck/ahash |
| aho-corasick | 1.1.5 | Unlicense OR MIT | https://github.com/BurntSushi/aho-corasick |
| alloc-no-stdlib | 2.0.4 | BSD-3-Clause | https://github.com/dropbox/rust-alloc-no-stdlib |
| alloc-stdlib | 0.2.4 | BSD-3-Clause | https://github.com/dropbox/rust-alloc-no-stdlib |
| android_system_properties | 0.1.6 | MIT OR Apache-2.0 | https://github.com/nical/android_system_properties |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | https://github.com/dtolnay/anyhow |
| arrayvec | 0.7.8 | MIT OR Apache-2.0 | https://github.com/bluss/arrayvec |
| atk | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| atk-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/atomic-waker |
| autocfg | 1.5.1 | Apache-2.0 OR MIT | https://github.com/cuviper/autocfg |
| base64 | 0.21.7 | MIT OR Apache-2.0 | https://github.com/marshallpierce/rust-base64 |
| base64 | 0.22.1 | MIT OR Apache-2.0 | https://github.com/marshallpierce/rust-base64 |
| bit-set | 0.8.0 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-set |
| bit-vec | 0.8.0 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-vec |
| bitflags | 1.3.2 | MIT/Apache-2.0 | https://github.com/bitflags/bitflags |
| bitflags | 2.13.1 | MIT OR Apache-2.0 | https://github.com/bitflags/bitflags |
| blake3 | 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | https://github.com/BLAKE3-team/BLAKE3 |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| block2 | 0.6.2 | MIT | https://github.com/madsmtm/objc2 |
| brotli | 8.0.4 | BSD-3-Clause AND MIT | https://github.com/dropbox/rust-brotli |
| brotli-decompressor | 5.0.3 | BSD-3-Clause/MIT | https://github.com/dropbox/rust-brotli-decompressor |
| bs58 | 0.5.1 | MIT/Apache-2.0 | https://github.com/Nullus157/bs58-rs |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 | https://github.com/fitzgen/bumpalo |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/bytemuck |
| byteorder | 1.5.0 | Unlicense OR MIT | https://github.com/BurntSushi/byteorder |
| byteorder-lite | 0.1.0 | Unlicense OR MIT | https://github.com/image-rs/byteorder-lite |
| bytes | 1.12.1 | MIT | https://github.com/tokio-rs/bytes |
| cairo-rs | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| cairo-sys-rs | 0.18.2 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| camino | 1.2.5 | MIT OR Apache-2.0 | https://github.com/camino-rs/camino |
| cargo-platform | 0.1.9 | MIT OR Apache-2.0 | https://github.com/rust-lang/cargo |
| cargo_metadata | 0.19.2 | MIT | https://github.com/oli-obk/cargo_metadata |
| cargo_toml | 0.22.3 | Apache-2.0 OR MIT | https://gitlab.com/lib.rs/cargo_toml |
| cc | 1.4.4 | MIT OR Apache-2.0 | https://github.com/rust-lang/cc-rs |
| cesu8 | 1.1.0 | Apache-2.0/MIT | https://github.com/emk/cesu8-rs |
| cfb | 0.7.3 | MIT | https://github.com/mdsteele/rust-cfb |
| cfg-expr | 0.15.8 | MIT OR Apache-2.0 | https://github.com/EmbarkStudios/cfg-expr |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 | https://github.com/rust-lang/cfg-if |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/stream-ciphers |
| chrono | 0.4.45 | MIT OR Apache-2.0 | https://github.com/chronotope/chrono |
| color_quant | 1.1.0 | MIT | https://github.com/image-rs/color_quant.git |
| combine | 4.6.8 | MIT | https://github.com/Marwes/combine |
| constant_time_eq | 0.4.2 | CC0-1.0 OR MIT-0 OR Apache-2.0 | https://github.com/cesarb/constant_time_eq |
| cookie | 0.18.2 | MIT OR Apache-2.0 | https://github.com/SergioBenitez/cookie-rs |
| core-foundation | 0.10.1 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| core-graphics | 0.25.0 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| core-graphics-types | 0.2.0 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| cpufeatures | 0.3.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| crc32fast | 1.5.1 | MIT OR Apache-2.0 | https://github.com/srijs/rust-crc32fast |
| crossbeam-channel | 0.5.16 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crossbeam-utils | 0.8.22 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crunchy | 0.2.4 | MIT | https://github.com/eira-fransham/crunchy |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| cssparser | 0.36.0 | MPL-2.0 | https://github.com/servo/rust-cssparser |
| cssparser-macros | 0.6.1 | MPL-2.0 | https://github.com/servo/rust-cssparser |
| ctor | 0.8.0 | Apache-2.0 OR MIT | https://github.com/mmastrac/rust-ctor |
| ctor-proc-macro | 0.0.7 | Apache-2.0 OR MIT | https://github.com/mmastrac/rust-ctor |
| darling | 0.23.0 | MIT | https://github.com/TedDriggs/darling |
| darling_core | 0.23.0 | MIT | https://github.com/TedDriggs/darling |
| darling_macro | 0.23.0 | MIT | https://github.com/TedDriggs/darling |
| dbus | 0.9.12 | Apache-2.0/MIT | https://github.com/diwic/dbus-rs |
| defmt | 1.1.1 | MIT OR Apache-2.0 | https://github.com/knurling-rs/defmt |
| defmt-macros | 1.1.1 | MIT OR Apache-2.0 | https://github.com/knurling-rs/defmt |
| defmt-parser | 1.0.0 | MIT OR Apache-2.0 | https://github.com/knurling-rs/defmt |
| deranged | 0.5.8 | MIT OR Apache-2.0 | https://github.com/jhpratt/deranged |
| derive_more | 2.1.1 | MIT | https://github.com/JelteF/derive_more |
| derive_more-impl | 2.1.1 | MIT | https://github.com/JelteF/derive_more |
| digest | 0.10.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| dirs | 6.0.0 | MIT OR Apache-2.0 | https://github.com/soc/dirs-rs |
| dirs-sys | 0.5.0 | MIT OR Apache-2.0 | https://github.com/dirs-dev/dirs-sys-rs |
| dispatch2 | 0.3.1 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 | https://github.com/yaahc/displaydoc |
| dlopen2 | 0.8.2 | MIT | https://github.com/OpenByteDev/dlopen2 |
| dlopen2_derive | 0.4.3 | MIT | https://github.com/OpenByteDev/dlopen2 |
| dom_query | 0.27.0 | MIT | https://github.com/niklak/dom_query |
| dpi | 0.1.2 | Apache-2.0 AND MIT | https://github.com/rust-windowing/winit |
| dtoa | 1.0.11 | MIT OR Apache-2.0 | https://github.com/dtolnay/dtoa |
| dtoa-short | 0.3.5 | MPL-2.0 | https://github.com/upsuper/dtoa-short |
| dtor | 0.3.0 | Apache-2.0 OR MIT | https://github.com/mmastrac/rust-ctor |
| dtor-proc-macro | 0.0.6 | Apache-2.0 OR MIT | https://github.com/mmastrac/rust-ctor |
| dunce | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 | https://gitlab.com/kornelski/dunce |
| dyn-clone | 1.0.20 | MIT OR Apache-2.0 | https://github.com/dtolnay/dyn-clone |
| embed-resource | 3.0.11 | MIT | https://github.com/nabijaczleweli/rust-embed-resource |
| embed_plist | 1.2.2 | MIT OR Apache-2.0 | https://github.com/nvzqz/embed-plist-rs |
| equivalent | 1.0.2 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/equivalent |
| erased-serde | 0.4.10 | MIT OR Apache-2.0 | https://github.com/dtolnay/erased-serde |
| fallible-iterator | 0.3.0 | MIT/Apache-2.0 | https://github.com/sfackler/rust-fallible-iterator |
| fallible-streaming-iterator | 0.1.9 | MIT/Apache-2.0 | https://github.com/sfackler/fallible-streaming-iterator |
| fastrand | 2.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/fastrand |
| fax | 0.2.7 | MIT | https://github.com/pdf-rs/fax |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 | https://github.com/image-rs/fdeflate |
| field-offset | 0.3.6 | MIT OR Apache-2.0 | https://github.com/Diggsey/rust-field-offset |
| find-msvc-tools | 0.1.11 | MIT OR Apache-2.0 | https://github.com/rust-lang/cc-rs |
| flate2 | 1.1.10 | MIT OR Apache-2.0 | https://github.com/rust-lang/flate2-rs |
| fnv | 1.0.7 | Apache-2.0 / MIT | https://github.com/servo/rust-fnv |
| foldhash | 0.2.0 | Zlib | https://github.com/orlp/foldhash |
| foreign-types | 0.5.0 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| foreign-types-macros | 0.2.4 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| foreign-types-shared | 0.3.1 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| fsevent-sys | 4.1.0 | MIT | https://github.com/octplane/fsevent-rust/tree/master/fsevent-sys |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-core | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-executor | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-io | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-macro | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-task | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-util | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| gdk | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gdk-pixbuf | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gdk-pixbuf-sys | 0.18.0 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gdk-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gdkwayland-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gdkx11 | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gdkx11-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| generic-array | 0.14.7 | MIT | https://github.com/fizyk20/generic-array.git |
| gento-core | 0.1.0 | (workspace) | (このリポジトリ) |
| gento-db | 0.1.0 | (workspace) | (このリポジトリ) |
| gento-image | 0.1.0 | (workspace) | (このリポジトリ) |
| gento-tauri | 0.1.0 | (workspace) | (このリポジトリ) |
| getrandom | 0.2.17 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| getrandom | 0.3.4 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| gif | 0.14.2 | MIT OR Apache-2.0 | https://github.com/image-rs/image-gif |
| gio | 0.18.4 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gio-sys | 0.18.1 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| glib | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| glib-macros | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| glib-sys | 0.18.1 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| glob | 0.3.4 | MIT OR Apache-2.0 | https://github.com/rust-lang/glob |
| gobject-sys | 0.18.0 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gtk | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gtk-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gtk3-macros | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| half | 2.7.1 | MIT OR Apache-2.0 | https://github.com/VoidStarKat/half-rs |
| hashbrown | 0.12.3 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.14.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashlink | 0.9.1 | MIT OR Apache-2.0 | https://github.com/kyren/hashlink |
| heck | 0.4.1 | MIT OR Apache-2.0 | https://github.com/withoutboats/heck |
| heck | 0.5.0 | MIT OR Apache-2.0 | https://github.com/withoutboats/heck |
| hex | 0.4.3 | MIT OR Apache-2.0 | https://github.com/KokaKiwi/rust-hex |
| html5ever | 0.38.0 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| http | 1.5.0 | MIT OR Apache-2.0 | https://github.com/hyperium/http |
| http-body | 1.1.0 | MIT | https://github.com/hyperium/http-body |
| http-body-util | 0.1.5 | MIT | https://github.com/hyperium/http-body |
| httparse | 1.10.1 | MIT OR Apache-2.0 | https://github.com/seanmonstar/httparse |
| hyper | 1.11.0 | MIT | https://github.com/hyperium/hyper |
| hyper-util | 0.1.20 | MIT | https://github.com/hyperium/hyper-util |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 | https://github.com/strawlab/iana-time-zone |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 | https://github.com/strawlab/iana-time-zone |
| ico | 0.5.0 | MIT | https://github.com/mdsteele/rust-ico |
| icu_collections | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_locale_core | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_normalizer | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_normalizer_data | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_properties | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_properties_data | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_provider | 2.3.1 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| ident_case | 1.0.1 | MIT/Apache-2.0 | https://github.com/TedDriggs/ident_case |
| idna | 1.1.0 | MIT OR Apache-2.0 | https://github.com/servo/rust-url/ |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT | https://github.com/hsivonen/idna_adapter |
| image | 0.25.10 | MIT OR Apache-2.0 | https://github.com/image-rs/image |
| image-webp | 0.2.4 | MIT OR Apache-2.0 | https://github.com/image-rs/image-webp |
| indexmap | 1.9.3 | Apache-2.0 OR MIT | https://github.com/bluss/indexmap |
| indexmap | 2.14.0 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/indexmap |
| infer | 0.19.0 | MIT | https://github.com/bojand/infer |
| inotify | 0.11.5 | ISC | https://github.com/hannobraun/inotify-rs |
| inotify-sys | 0.1.8 | ISC | https://github.com/hannobraun/inotify-sys |
| ipnet | 2.12.1 | MIT OR Apache-2.0 | https://github.com/krisprice/ipnet |
| itoa | 1.0.18 | MIT OR Apache-2.0 | https://github.com/dtolnay/itoa |
| javascriptcore-rs | 1.1.2 | MIT | https://github.com/tauri-apps/javascriptcore-rs |
| javascriptcore-rs-sys | 1.1.1 | MIT | https://github.com/tauri-apps/javascriptcore-rs |
| jiff | 0.2.35 | Unlicense OR MIT | https://github.com/BurntSushi/jiff |
| jiff-core | 0.1.0 | Unlicense OR MIT | https://github.com/BurntSushi/jiff |
| jiff-static | 0.2.35 | Unlicense OR MIT | https://github.com/BurntSushi/jiff |
| jiff-tzdb | 0.1.8 | Unlicense OR MIT | https://github.com/BurntSushi/jiff |
| jiff-tzdb-platform | 0.1.3 | Unlicense OR MIT | https://github.com/BurntSushi/jiff |
| jni | 0.21.1 | MIT/Apache-2.0 | https://github.com/jni-rs/jni-rs |
| jni-sys | 0.3.1 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-sys |
| jni-sys | 0.4.1 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-sys |
| jni-sys-macros | 0.4.1 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-sys |
| jobserver | 0.1.35 | MIT OR Apache-2.0 | https://github.com/rust-lang/jobserver-rs |
| js-sys | 0.3.104 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys |
| json-patch | 3.0.1 | MIT/Apache-2.0 | https://github.com/idubrov/json-patch |
| jsonptr | 0.6.3 | MIT OR Apache-2.0 | https://github.com/chanced/jsonptr |
| kamadak-exif | 0.5.5 | BSD-2-Clause | https://github.com/kamadak/exif-rs |
| keyboard-types | 0.7.0 | MIT OR Apache-2.0 | https://github.com/pyfisch/keyboard-types |
| kqueue | 1.2.1 | MIT | https://gitlab.com/rust-kqueue/rust-kqueue |
| kqueue-sys | 1.1.2 | MIT | https://gitlab.com/rust-kqueue/rust-kqueue-sys |
| lazy_static | 1.5.0 | MIT OR Apache-2.0 | https://github.com/rust-lang-nursery/lazy-static.rs |
| libappindicator | 0.9.0 | Apache-2.0 OR MIT | https://crates.io/crates/libappindicator |
| libappindicator-sys | 0.9.0 | Apache-2.0 OR MIT | https://crates.io/crates/libappindicator-sys |
| libc | 0.2.189 | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| libdbus-sys | 0.2.7 | Apache-2.0/MIT | https://github.com/diwic/dbus-rs |
| libloading | 0.7.4 | ISC | https://github.com/nagisa/rust_libloading/ |
| libredox | 0.1.21 | MIT | https://gitlab.redox-os.org/redox-os/libredox.git |
| libsqlite3-sys | 0.28.0 | MIT | https://github.com/rusqlite/rusqlite |
| libwebp-sys | 0.9.6 | MIT | https://github.com/NoXF/libwebp-sys |
| litemap | 0.8.3 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| lock_api | 0.4.14 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| log | 0.4.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/log |
| markup5ever | 0.38.0 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| matchers | 0.2.0 | MIT | https://github.com/hawkw/matchers |
| memchr | 2.8.3 | Unlicense OR MIT | https://github.com/BurntSushi/memchr |
| memoffset | 0.9.1 | MIT | https://github.com/Gilnaa/memoffset |
| mime | 0.3.17 | MIT OR Apache-2.0 | https://github.com/hyperium/mime |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| mio | 1.2.2 | MIT | https://github.com/tokio-rs/mio |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 | https://github.com/awxkee/moxcms.git |
| muda | 0.19.3 | Apache-2.0 OR MIT | https://github.com/tauri-apps/muda |
| mutate_once | 0.1.2 | BSD-2-Clause | https://github.com/kamadak/mutate_once-rs |
| ndk | 0.9.0 | MIT OR Apache-2.0 | https://github.com/rust-mobile/ndk |
| ndk-sys | 0.6.0+11769913 | MIT OR Apache-2.0 | https://github.com/rust-mobile/ndk |
| new_debug_unreachable | 1.0.6 | MIT | https://github.com/mbrubeck/rust-debug-unreachable |
| notify | 8.2.0 | CC0-1.0 | https://github.com/notify-rs/notify.git |
| notify-types | 2.1.0 | MIT OR Apache-2.0 | https://github.com/notify-rs/notify.git |
| nu-ansi-term | 0.50.3 | MIT | https://github.com/nushell/nu-ansi-term |
| num-conv | 0.2.2 | MIT OR Apache-2.0 | https://github.com/jhpratt/num-conv |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | https://github.com/rust-num/num-traits |
| num_enum | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 | https://github.com/illicitonion/num_enum |
| num_enum_derive | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 | https://github.com/illicitonion/num_enum |
| objc2 | 0.6.4 | MIT | https://github.com/madsmtm/objc2 |
| objc2-app-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-cloud-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-data | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-foundation | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-graphics | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-image | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-location | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-text | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-encode | 4.1.0 | MIT | https://github.com/madsmtm/objc2 |
| objc2-exception-helper | 0.1.1 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-foundation | 0.3.2 | MIT | https://github.com/madsmtm/objc2 |
| objc2-io-surface | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-quartz-core | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-ui-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-user-notifications | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-web-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | https://github.com/matklad/once_cell |
| option-ext | 0.2.0 | MPL-2.0 | https://github.com/soc/option-ext.git |
| pango | 0.18.3 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| pango-sys | 0.18.0 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url/ |
| phf | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_codegen | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_generator | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_macros | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_shared | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT | https://github.com/taiki-e/pin-project-lite |
| pkg-config | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/pkg-config-rs |
| plist | 1.10.0 | MIT | https://github.com/ebarnard/rust-plist/ |
| png | 0.17.16 | MIT OR Apache-2.0 | https://github.com/image-rs/image-png |
| png | 0.18.1 | MIT OR Apache-2.0 | https://github.com/image-rs/image-png |
| portable-atomic | 1.15.0 | Apache-2.0 OR MIT | https://github.com/taiki-e/portable-atomic |
| portable-atomic-util | 0.2.7 | Apache-2.0 OR MIT | https://github.com/taiki-e/portable-atomic-util |
| potential_utf | 0.1.6 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| powerfmt | 0.2.0 | MIT OR Apache-2.0 | https://github.com/jhpratt/powerfmt |
| precomputed-hash | 0.1.1 | MIT | https://github.com/emilio/precomputed-hash |
| proc-macro-crate | 1.3.1 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro-crate | 2.0.2 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro-crate | 3.5.0 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro-error | 1.0.4 | MIT OR Apache-2.0 | https://gitlab.com/CreepySkeleton/proc-macro-error |
| proc-macro-error-attr | 1.0.4 | MIT OR Apache-2.0 | https://gitlab.com/CreepySkeleton/proc-macro-error |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| pxfm | 0.1.30 | BSD-3-Clause OR Apache-2.0 | https://github.com/awxkee/pxfm |
| quick-error | 2.0.1 | MIT/Apache-2.0 | http://github.com/tailhook/quick-error |
| quick-xml | 0.41.0 | MIT | https://github.com/tafia/quick-xml |
| quote | 1.0.47 | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| r-efi | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | https://github.com/r-efi/r-efi |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | https://github.com/r-efi/r-efi |
| rand | 0.10.2 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rand_core | 0.10.1 | MIT OR Apache-2.0 | https://github.com/rust-random/rand_core |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib | https://github.com/rust-windowing/raw-window-handle |
| redox_syscall | 0.5.18 | MIT | https://gitlab.redox-os.org/redox-os/syscall |
| redox_users | 0.5.2 | MIT | https://gitlab.redox-os.org/redox-os/users |
| ref-cast | 1.0.27 | MIT OR Apache-2.0 | https://github.com/dtolnay/ref-cast |
| ref-cast-impl | 1.0.27 | MIT OR Apache-2.0 | https://github.com/dtolnay/ref-cast |
| regex | 1.13.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| regex-automata | 0.4.18 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| reqwest | 0.13.4 | MIT OR Apache-2.0 | https://github.com/seanmonstar/reqwest |
| rusqlite | 0.31.0 | MIT | https://github.com/rusqlite/rusqlite |
| rustc-hash | 2.1.3 | Apache-2.0 OR MIT | https://github.com/rust-lang/rustc-hash |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 | https://github.com/djc/rustc-version-rs |
| rustversion | 1.0.23 | MIT OR Apache-2.0 | https://github.com/dtolnay/rustversion |
| same-file | 1.0.6 | Unlicense/MIT | https://github.com/BurntSushi/same-file |
| schemars | 0.8.22 | MIT | https://github.com/GREsau/schemars |
| schemars | 0.9.0 | MIT | https://github.com/GREsau/schemars |
| schemars | 1.2.2 | MIT | https://github.com/GREsau/schemars |
| schemars_derive | 0.8.22 | MIT | https://github.com/GREsau/schemars |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 | https://github.com/bluss/scopeguard |
| selectors | 0.36.1 | MPL-2.0 | https://github.com/servo/stylo |
| semver | 1.0.28 | MIT OR Apache-2.0 | https://github.com/dtolnay/semver |
| serde | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde-untagged | 0.1.9 | MIT OR Apache-2.0 | https://github.com/dtolnay/serde-untagged |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_derive_internals | 0.29.1 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_json | 1.0.151 | MIT OR Apache-2.0 | https://github.com/serde-rs/json |
| serde_repr | 0.1.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/serde-repr |
| serde_spanned | 0.6.9 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| serde_spanned | 1.1.1 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| serde_with | 3.22.0 | MIT OR Apache-2.0 | https://github.com/jonasbb/serde_with/ |
| serde_with_macros | 3.22.0 | MIT OR Apache-2.0 | https://github.com/jonasbb/serde_with/ |
| serialize-to-javascript | 0.1.2 | MIT OR Apache-2.0 | https://github.com/chippers/serialize-to-javascript |
| serialize-to-javascript-impl | 0.1.2 | MIT OR Apache-2.0 | https://github.com/chippers/serialize-to-javascript |
| servo_arc | 0.4.3 | MIT OR Apache-2.0 | https://github.com/servo/stylo |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| sharded-slab | 0.1.7 | MIT | https://github.com/hawkw/sharded-slab |
| shlex | 2.0.1 | MIT OR Apache-2.0 | https://github.com/comex/rust-shlex |
| simd-adler32 | 0.3.10 | MIT | https://github.com/mcountryman/simd-adler32 |
| siphasher | 1.0.3 | MIT/Apache-2.0 | https://github.com/jedisct1/rust-siphash |
| slab | 0.4.12 | MIT | https://github.com/tokio-rs/slab |
| smallvec | 1.15.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-smallvec |
| socket2 | 0.6.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/socket2 |
| softbuffer | 0.4.8 | MIT OR Apache-2.0 | https://github.com/rust-windowing/softbuffer |
| soup3 | 0.5.0 | MIT | https://gitlab.gnome.org/World/Rust/soup3-rs |
| soup3-sys | 0.5.0 | MIT | https://gitlab.gnome.org/World/Rust/soup3-rs |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 | https://github.com/storyyeller/stable_deref_trait |
| string_cache | 0.9.0 | MIT OR Apache-2.0 | https://github.com/servo/string-cache |
| string_cache_codegen | 0.6.1 | MIT OR Apache-2.0 | https://github.com/servo/string-cache |
| strsim | 0.11.1 | MIT | https://github.com/rapidfuzz/strsim-rs |
| swift-rs | 1.0.8 | MIT OR Apache-2.0 | https://github.com/Brendonovich/swift-rs |
| syn | 1.0.109 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| syn | 2.0.119 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| syn | 3.0.4 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| sync_wrapper | 1.0.2 | Apache-2.0 | https://github.com/Actyx/sync_wrapper |
| synstructure | 0.13.2 | MIT | https://github.com/mystor/synstructure |
| system-deps | 6.2.2 | MIT OR Apache-2.0 | https://github.com/gdesmott/system-deps |
| tao | 0.35.3 | Apache-2.0 | https://github.com/tauri-apps/tao |
| tao-macros | 0.1.4 | MIT OR Apache-2.0 | https://github.com/tauri-apps/tao |
| target-lexicon | 0.12.16 | Apache-2.0 WITH LLVM-exception | https://github.com/bytecodealliance/target-lexicon |
| tauri | 2.11.5 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| tauri-build | 2.6.3 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| tauri-codegen | 2.6.3 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| tauri-macros | 2.6.3 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| tauri-runtime | 2.11.3 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| tauri-runtime-wry | 2.11.4 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| tauri-utils | 2.9.3 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri |
| tauri-winres | 0.3.6 | MIT | https://github.com/tauri-apps/winres |
| tendril | 0.5.1 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| thiserror | 1.0.69 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror | 2.0.20 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror-impl | 1.0.69 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror-impl | 2.0.20 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thread_local | 1.1.10 | MIT OR Apache-2.0 | https://github.com/Amanieu/thread_local-rs |
| tiff | 0.11.3 | MIT | https://github.com/image-rs/image-tiff |
| time | 0.3.55 | MIT OR Apache-2.0 | https://github.com/time-rs/time |
| time-core | 0.1.9 | MIT OR Apache-2.0 | https://github.com/time-rs/time |
| time-macros | 0.2.32 | MIT OR Apache-2.0 | https://github.com/time-rs/time |
| tinystr | 0.8.4 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| tinyvec | 1.12.0 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/tinyvec |
| tinyvec_macros | 0.1.1 | MIT OR Apache-2.0 OR Zlib | https://github.com/Soveu/tinyvec_macros |
| tokio | 1.53.1 | MIT | https://github.com/tokio-rs/tokio |
| tokio-util | 0.7.19 | MIT | https://github.com/tokio-rs/tokio |
| toml | 0.8.2 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml | 0.9.12+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml | 1.1.4+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 0.6.3 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 0.7.5+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.19.15 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.20.2 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.25.13+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_parser | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_writer | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| tower | 0.5.3 | MIT | https://github.com/tower-rs/tower |
| tower-http | 0.6.11 | MIT | https://github.com/tower-rs/tower-http |
| tower-layer | 0.3.3 | MIT | https://github.com/tower-rs/tower |
| tower-service | 0.3.3 | MIT | https://github.com/tower-rs/tower |
| tracing | 0.1.44 | MIT | https://github.com/tokio-rs/tracing |
| tracing-attributes | 0.1.31 | MIT | https://github.com/tokio-rs/tracing |
| tracing-core | 0.1.36 | MIT | https://github.com/tokio-rs/tracing |
| tracing-log | 0.2.0 | MIT | https://github.com/tokio-rs/tracing |
| tracing-subscriber | 0.3.23 | MIT | https://github.com/tokio-rs/tracing |
| tray-icon | 0.24.2 | MIT OR Apache-2.0 | https://github.com/tauri-apps/tray-icon |
| try-lock | 0.2.5 | MIT | https://github.com/seanmonstar/try-lock |
| twox-hash | 2.1.4 | MIT | https://github.com/shepmaster/twox-hash |
| typeid | 1.0.3 | MIT OR Apache-2.0 | https://github.com/dtolnay/typeid |
| typenum | 1.20.1 | MIT OR Apache-2.0 | https://github.com/paholg/typenum |
| unic-char-property | 0.9.0 | MIT/Apache-2.0 | https://github.com/open-i18n/rust-unic/ |
| unic-char-range | 0.9.0 | MIT/Apache-2.0 | https://github.com/open-i18n/rust-unic/ |
| unic-common | 0.9.0 | MIT/Apache-2.0 | https://github.com/open-i18n/rust-unic/ |
| unic-ucd-ident | 0.9.0 | MIT/Apache-2.0 | https://github.com/open-i18n/rust-unic/ |
| unic-ucd-version | 0.9.0 | MIT/Apache-2.0 | https://github.com/open-i18n/rust-unic/ |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-segmentation |
| url | 2.5.8 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| urlpattern | 0.3.0 | MIT | https://github.com/denoland/rust-urlpattern |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT | https://github.com/hsivonen/utf8_iter |
| uuid | 1.26.0 | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| valuable | 0.1.1 | MIT | https://github.com/tokio-rs/valuable |
| vcpkg | 0.2.15 | MIT/Apache-2.0 | https://github.com/mcgoo/vcpkg-rs |
| version-compare | 0.2.1 | MIT | https://gitlab.com/timvisee/version-compare |
| version_check | 0.9.5 | MIT/Apache-2.0 | https://github.com/SergioBenitez/version_check |
| vswhom | 0.1.0 | MIT | https://github.com/nabijaczleweli/vswhom.rs |
| vswhom-sys | 0.1.3 | MIT | https://github.com/nabijaczleweli/vswhom-sys.rs |
| walkdir | 2.5.0 | Unlicense/MIT | https://github.com/BurntSushi/walkdir |
| want | 0.3.1 | MIT | https://github.com/seanmonstar/want |
| wasi | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/wasi |
| wasip2 | 1.0.4+wasi-0.2.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/wasi-rs |
| wasm-bindgen | 0.2.127 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen |
| wasm-bindgen-futures | 0.4.77 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/futures |
| wasm-bindgen-macro | 0.2.127 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro |
| wasm-bindgen-macro-support | 0.2.127 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro-support |
| wasm-bindgen-shared | 0.2.127 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared |
| wasm-streams | 0.5.0 | MIT OR Apache-2.0 | https://github.com/MattiasBuelens/wasm-streams/ |
| web-sys | 0.3.104 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys |
| web_atoms | 0.2.6 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| webkit2gtk | 2.0.2 | MIT | https://github.com/tauri-apps/webkit2gtk-rs |
| webkit2gtk-sys | 2.0.2 | MIT | https://github.com/tauri-apps/webkit2gtk-rs |
| webp | 0.3.1 | MIT OR Apache-2.0 | https://github.com/jaredforth/webp.git |
| webview2-com | 0.38.2 | MIT | https://github.com/wravery/webview2-rs |
| webview2-com-macros | 0.8.1 | MIT | https://github.com/wravery/webview2-rs |
| webview2-com-sys | 0.38.2 | MIT | https://github.com/wravery/webview2-rs |
| weezl | 0.1.12 | MIT OR Apache-2.0 | https://github.com/image-rs/weezl |
| winapi | 0.3.9 | MIT/Apache-2.0 | https://github.com/retep998/winapi-rs |
| winapi-i686-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 | https://github.com/retep998/winapi-rs |
| winapi-util | 0.1.11 | Unlicense OR MIT | https://github.com/BurntSushi/winapi-util |
| winapi-x86_64-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 | https://github.com/retep998/winapi-rs |
| window-vibrancy | 0.6.0 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri-plugin-vibrancy |
| windows | 0.61.3 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-collections | 0.2.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-core | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-core | 0.62.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-future | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-link | 0.1.3 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-numerics | 0.2.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-result | 0.3.4 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-result | 0.4.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-strings | 0.4.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.45.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.59.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.60.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-targets | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-targets | 0.53.5 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-threading | 0.1.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-version | 0.1.7 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_gnullvm | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_gnullvm | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_gnullvm | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_msvc | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_msvc | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_msvc | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnu | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnu | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnu | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnullvm | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnullvm | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_msvc | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_msvc | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_msvc | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnu | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnu | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnullvm | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnullvm | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnullvm | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_msvc | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_msvc | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_msvc | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| winnow | 0.5.40 | MIT | https://github.com/winnow-rs/winnow |
| winnow | 0.7.15 | MIT | https://github.com/winnow-rs/winnow |
| winnow | 1.0.4 | MIT | https://github.com/winnow-rs/winnow |
| winreg | 0.55.0 | MIT | https://github.com/gentoo90/winreg-rs |
| wit-bindgen | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/wit-bindgen |
| writeable | 0.6.4 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| wry | 0.55.1 | Apache-2.0 OR MIT | https://github.com/tauri-apps/wry |
| x11 | 2.21.0 | MIT | https://github.com/AltF02/x11-rs.git |
| x11-dl | 2.21.0 | MIT | https://github.com/AltF02/x11-rs.git |
| yoke | 0.8.3 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| yoke-derive | 0.8.2 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerocopy | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zerocopy-derive | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zerofrom | 0.1.8 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerofrom-derive | 0.1.7 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerotrie | 0.2.5 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerovec | 0.11.8 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerovec-derive | 0.11.6 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zlib-rs | 0.6.7 | Zlib | https://github.com/trifectatechfoundation/zlib-rs |
| zmij | 1.0.23 | MIT | https://github.com/dtolnay/zmij |
| zune-core | 0.5.3 | MIT OR Apache-2.0 OR Zlib | https://github.com/etemesi254/zune-image |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib | https://github.com/etemesi254/zune-image/tree/dev/crates/zune-jpeg |

## TypeScript / Svelte (apps/desktop)

| パッケージ | バージョン | license | 入手元 |
|---|---|---|---|
| @eslint-community/eslint-utils | 4.10.1 | MIT | https://github.com/eslint-community/eslint-utils#readme |
| @eslint-community/regexpp | 4.12.2 | MIT | https://github.com/eslint-community/regexpp#readme |
| @eslint/config-array | 0.23.5 | Apache-2.0 | https://github.com/eslint/rewrite/tree/main/packages/config-array#readme |
| @eslint/config-helpers | 0.7.0 | Apache-2.0 | https://github.com/eslint/rewrite/tree/main/packages/config-helpers#readme |
| @eslint/core | 1.2.1 | Apache-2.0 | https://github.com/eslint/rewrite/tree/main/packages/core#readme |
| @eslint/js | 10.0.1 | MIT | https://eslint.org |
| @eslint/object-schema | 3.0.5 | Apache-2.0 | https://github.com/eslint/rewrite/tree/main/packages/object-schema#readme |
| @eslint/plugin-kit | 0.7.2 | Apache-2.0 | https://github.com/eslint/rewrite/tree/main/packages/plugin-kit#readme |
| @humanfs/core | 0.19.2 | Apache-2.0 | https://github.com/humanwhocodes/humanfs#readme |
| @humanfs/node | 0.16.8 | Apache-2.0 | https://github.com/humanwhocodes/humanfs#readme |
| @humanfs/types | 0.15.0 | Apache-2.0 | https://github.com/humanwhocodes/humanfs#readme |
| @humanwhocodes/module-importer | 1.0.1 | Apache-2.0 | https://github.com/humanwhocodes/module-importer#readme |
| @humanwhocodes/retry | 0.4.3 | Apache-2.0 | https://github.com/humanwhocodes/retry#readme |
| @jridgewell/gen-mapping | 0.3.13 | MIT | https://github.com/jridgewell/sourcemaps/tree/main/packages/gen-mapping |
| @jridgewell/remapping | 2.3.5 | MIT | https://github.com/jridgewell/sourcemaps/tree/main/packages/remapping |
| @jridgewell/resolve-uri | 3.1.2 | MIT | https://github.com/jridgewell/resolve-uri#readme |
| @jridgewell/sourcemap-codec | 1.6.0 | MIT | https://github.com/jridgewell/sourcemaps/tree/main/packages/sourcemap-codec |
| @jridgewell/trace-mapping | 0.3.31 | MIT | https://github.com/jridgewell/sourcemaps/tree/main/packages/trace-mapping |
| @oxc-project/types | 0.147.0 | MIT | https://oxc.rs |
| @rolldown/binding-linux-x64-gnu | 1.2.6 | MIT | https://rolldown.rs/ |
| @rolldown/pluginutils | 1.0.1 | MIT | https://github.com/rolldown/plugins/tree/main/packages/pluginutils#readme |
| @standard-schema/spec | 1.1.0 | MIT | https://standardschema.dev |
| @sveltejs/acorn-typescript | 1.0.13 | MIT | https://github.com/sveltejs/acorn-typescript#readme |
| @sveltejs/load-config | 0.2.3 | MIT | https://github.com/sveltejs/language-tools#readme |
| @sveltejs/vite-plugin-svelte | 7.3.0 | MIT | https://github.com/sveltejs/vite-plugin-svelte#readme |
| @tanstack/svelte-virtual | 3.13.36 | MIT | https://tanstack.com/virtual |
| @tanstack/virtual-core | 3.17.8 | MIT | https://tanstack.com/virtual |
| @tauri-apps/api | 2.11.1 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri#readme |
| @tauri-apps/cli | 2.11.4 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri#readme |
| @tauri-apps/cli-linux-x64-gnu | 2.11.4 | Apache-2.0 OR MIT | https://github.com/tauri-apps/tauri#readme |
| @types/chai | 5.2.3 | MIT | https://github.com/DefinitelyTyped/DefinitelyTyped/tree/master/types/chai |
| @types/deep-eql | 4.0.2 | MIT | https://github.com/DefinitelyTyped/DefinitelyTyped/tree/master/types/deep-eql |
| @types/esrecurse | 4.3.1 | MIT | https://github.com/DefinitelyTyped/DefinitelyTyped/tree/master/types/esrecurse |
| @types/estree | 1.0.9 | MIT | https://github.com/DefinitelyTyped/DefinitelyTyped/tree/master/types/estree |
| @types/json-schema | 7.0.15 | MIT | https://github.com/DefinitelyTyped/DefinitelyTyped/tree/master/types/json-schema |
| @types/trusted-types | 2.0.7 | MIT | https://github.com/DefinitelyTyped/DefinitelyTyped/tree/master/types/trusted-types |
| @typescript-eslint/eslint-plugin | 8.68.0 | MIT | https://typescript-eslint.io/packages/eslint-plugin |
| @typescript-eslint/parser | 8.68.0 | MIT | https://typescript-eslint.io/packages/parser |
| @typescript-eslint/project-service | 8.68.0 | MIT | https://typescript-eslint.io |
| @typescript-eslint/scope-manager | 8.68.0 | MIT | https://typescript-eslint.io/packages/scope-manager |
| @typescript-eslint/tsconfig-utils | 8.68.0 | MIT | https://typescript-eslint.io |
| @typescript-eslint/type-utils | 8.68.0 | MIT | https://typescript-eslint.io |
| @typescript-eslint/types | 8.68.0 | MIT | https://typescript-eslint.io |
| @typescript-eslint/typescript-estree | 8.68.0 | MIT | https://typescript-eslint.io/packages/typescript-estree |
| @typescript-eslint/utils | 8.68.0 | MIT | https://typescript-eslint.io/packages/utils |
| @typescript-eslint/visitor-keys | 8.68.0 | MIT | https://typescript-eslint.io |
| @vitest/expect | 4.1.11 | MIT | https://vitest.dev/api/expect |
| @vitest/mocker | 4.1.11 | MIT | https://github.com/vitest-dev/vitest/tree/main/packages/mocker |
| @vitest/pretty-format | 4.1.11 | MIT | https://github.com/vitest-dev/vitest/tree/main/packages/pretty-format |
| @vitest/runner | 4.1.11 | MIT | https://vitest.dev/api/advanced/runner |
| @vitest/snapshot | 4.1.11 | MIT | https://vitest.dev/guide/snapshot |
| @vitest/spy | 4.1.11 | MIT | https://vitest.dev/api/mock |
| @vitest/utils | 4.1.11 | MIT | https://github.com/vitest-dev/vitest/tree/main/packages/utils |
| acorn | 8.18.0 | MIT | https://github.com/acornjs/acorn |
| acorn-jsx | 5.3.2 | MIT | https://github.com/acornjs/acorn-jsx |
| ajv | 6.15.0 | MIT | https://github.com/ajv-validator/ajv |
| aria-query | 5.3.1 | Apache-2.0 | https://github.com/A11yance/aria-query#readme |
| assertion-error | 2.0.1 | MIT | https://github.com/chaijs/assertion-error#readme |
| axobject-query | 4.1.0 | Apache-2.0 | https://github.com/A11yance/axobject-query#readme |
| balanced-match | 4.0.4 | MIT | https://github.com/juliangruber/balanced-match#readme |
| brace-expansion | 5.0.9 | MIT | https://github.com/juliangruber/brace-expansion#readme |
| chai | 6.2.2 | MIT | http://chaijs.com |
| chokidar | 4.0.3 | MIT | https://github.com/paulmillr/chokidar |
| clsx | 2.1.1 | MIT | https://github.com/lukeed/clsx#readme |
| convert-source-map | 2.0.0 | MIT | https://github.com/thlorenz/convert-source-map |
| cross-spawn | 7.0.6 | MIT | https://github.com/moxystudio/node-cross-spawn |
| cssesc | 3.0.0 | MIT | https://mths.be/cssesc |
| debug | 4.4.3 | MIT | https://github.com/debug-js/debug#readme |
| deep-is | 0.1.4 | MIT | https://github.com/thlorenz/deep-is#readme |
| deepmerge | 4.3.1 | MIT | https://github.com/TehShrike/deepmerge |
| detect-libc | 2.1.2 | Apache-2.0 | https://github.com/lovell/detect-libc#readme |
| devalue | 5.9.2 | MIT | https://github.com/sveltejs/devalue#readme |
| es-module-lexer | 2.3.2 | MIT | https://github.com/guybedford/es-module-lexer#readme |
| escape-string-regexp | 4.0.0 | MIT | https://github.com/sindresorhus/escape-string-regexp#readme |
| eslint | 10.9.1 | MIT | https://eslint.org |
| eslint-plugin-svelte | 3.23.0 | MIT | https://sveltejs.github.io/eslint-plugin-svelte |
| eslint-scope | 8.4.0, 9.1.2 | BSD-2-Clause | https://github.com/eslint/js/blob/main/packages/eslint-scope/README.md |
| eslint-visitor-keys | 3.4.3, 4.2.1, 5.0.1 | Apache-2.0 | https://github.com/eslint/js/blob/main/packages/eslint-visitor-keys/README.md |
| esm-env | 1.2.2 | MIT | https://github.com/benmccann/esm-env |
| espree | 10.4.0, 11.2.0 | BSD-2-Clause | https://github.com/eslint/js/blob/main/packages/espree/README.md |
| esquery | 1.7.0 | BSD-3-Clause | https://github.com/estools/esquery/ |
| esrap | 2.3.6 | MIT | https://github.com/sveltejs/esrap#readme |
| esrecurse | 4.3.0 | BSD-2-Clause | https://github.com/estools/esrecurse |
| estraverse | 5.3.0 | BSD-2-Clause | https://github.com/estools/estraverse |
| estree-walker | 3.0.3 | MIT | https://github.com/Rich-Harris/estree-walker#readme |
| esutils | 2.0.3 | BSD-2-Clause | https://github.com/estools/esutils |
| expect-type | 1.4.0 | Apache-2.0 | https://github.com/mmkal/expect-type#readme |
| fast-deep-equal | 3.1.3 | MIT | https://github.com/epoberezkin/fast-deep-equal#readme |
| fast-json-stable-stringify | 2.1.0 | MIT | https://github.com/epoberezkin/fast-json-stable-stringify |
| fast-levenshtein | 2.0.6 | MIT | https://github.com/hiddentao/fast-levenshtein#readme |
| fdir | 6.5.0 | MIT | https://github.com/thecodrr/fdir#readme |
| file-entry-cache | 8.0.0 | MIT | https://github.com/jaredwray/file-entry-cache#readme |
| find-up | 5.0.0 | MIT | https://github.com/sindresorhus/find-up#readme |
| flat-cache | 4.0.1 | MIT | https://github.com/jaredwray/flat-cache#readme |
| flatted | 3.4.4 | ISC | https://github.com/WebReflection/flatted#readme |
| glob-parent | 6.0.2 | ISC | https://github.com/gulpjs/glob-parent#readme |
| globals | 16.5.0, 17.11.0 | MIT | https://github.com/sindresorhus/globals#readme |
| ignore | 5.3.2, 7.0.6 | MIT | https://github.com/kaelzhang/node-ignore#readme |
| imurmurhash | 0.1.4 | MIT | https://github.com/jensyt/imurmurhash-js |
| is-extglob | 2.1.1 | MIT | https://github.com/jonschlinkert/is-extglob |
| is-glob | 4.0.3 | MIT | https://github.com/micromatch/is-glob |
| is-reference | 3.0.3 | MIT | https://github.com/Rich-Harris/is-reference#readme |
| isexe | 2.0.0 | ISC | https://github.com/isaacs/isexe#readme |
| json-buffer | 3.0.1 | MIT | https://github.com/dominictarr/json-buffer |
| json-schema-traverse | 0.4.1 | MIT | https://github.com/epoberezkin/json-schema-traverse#readme |
| json-stable-stringify-without-jsonify | 1.0.1 | MIT | https://github.com/samn/json-stable-stringify |
| keyv | 4.5.4 | MIT | https://github.com/jaredwray/keyv |
| known-css-properties | 0.37.0 | MIT | https://github.com/known-css/known-css-properties#readme |
| levn | 0.4.1 | MIT | https://github.com/gkz/levn |
| lightningcss | 1.33.0 | MPL-2.0 | https://github.com/parcel-bundler/lightningcss#readme |
| lightningcss-linux-x64-gnu | 1.33.0 | MPL-2.0 | https://github.com/parcel-bundler/lightningcss#readme |
| lilconfig | 2.1.0 | MIT | https://github.com/antonk52/lilconfig#readme |
| locate-character | 3.0.0 | MIT | https://gitlab.com/Rich-Harris/locate-character#README |
| locate-path | 6.0.0 | MIT | https://github.com/sindresorhus/locate-path#readme |
| magic-string | 0.30.21, 1.2.3 | MIT | https://github.com/Rich-Harris/magic-string#readme |
| minimatch | 10.2.6 | BlueOak-1.0.0 | https://github.com/isaacs/minimatch#readme |
| mri | 1.2.0 | MIT | https://github.com/lukeed/mri#readme |
| ms | 2.1.3 | MIT | https://github.com/vercel/ms#readme |
| nanoid | 3.3.18 | MIT | https://github.com/ai/nanoid#readme |
| natural-compare | 1.4.0 | MIT | https://github.com/litejs/natural-compare-lite#readme |
| obug | 2.1.4 | MIT | https://github.com/sxzz/obug#readme |
| optionator | 0.9.4 | MIT | https://github.com/gkz/optionator |
| p-limit | 3.1.0 | MIT | https://github.com/sindresorhus/p-limit#readme |
| p-locate | 5.0.0 | MIT | https://github.com/sindresorhus/p-locate#readme |
| path-exists | 4.0.0 | MIT | https://github.com/sindresorhus/path-exists#readme |
| path-key | 3.1.1 | MIT | https://github.com/sindresorhus/path-key#readme |
| pathe | 2.0.3 | MIT | https://github.com/unjs/pathe#readme |
| picocolors | 1.1.1 | ISC | https://github.com/alexeyraspopov/picocolors#readme |
| picomatch | 4.0.7 | MIT | https://github.com/micromatch/picomatch |
| postcss | 8.5.26 | MIT | https://postcss.org/ |
| postcss-load-config | 3.1.4 | MIT | https://github.com/postcss/postcss-load-config#readme |
| postcss-safe-parser | 7.0.1 | MIT | https://github.com/postcss/postcss-safe-parser#readme |
| postcss-scss | 4.0.9 | MIT | https://github.com/postcss/postcss-scss#readme |
| postcss-selector-parser | 7.1.5 | MIT | https://github.com/postcss/postcss-selector-parser |
| prelude-ls | 1.2.1 | MIT | http://preludels.com |
| punycode | 2.3.1 | MIT | https://mths.be/punycode |
| readdirp | 4.1.2 | MIT | https://github.com/paulmillr/readdirp |
| rolldown | 1.2.6 | MIT | https://rolldown.rs/ |
| sade | 1.8.1 | MIT | https://github.com/lukeed/sade#readme |
| semver | 7.8.5 | ISC | https://github.com/npm/node-semver#readme |
| shebang-command | 2.0.0 | MIT | https://github.com/kevva/shebang-command#readme |
| shebang-regex | 3.0.0 | MIT | https://github.com/sindresorhus/shebang-regex#readme |
| siginfo | 2.0.0 | ISC | https://github.com/emilbayes/siginfo#readme |
| source-map-js | 1.2.1 | BSD-3-Clause | https://github.com/7rulnik/source-map-js |
| stackback | 0.0.2 | MIT | https://github.com/shtylman/node-stackback#readme |
| std-env | 4.2.0 | MIT | https://github.com/unjs/std-env#readme |
| svelte | 5.56.10 | MIT | https://svelte.dev |
| svelte-check | 4.7.6 | MIT | https://github.com/sveltejs/language-tools#readme |
| svelte-eslint-parser | 1.8.1 | MIT | https://github.com/sveltejs/svelte-eslint-parser#readme |
| tinybench | 2.9.0 | MIT | https://github.com/tinylibs/tinybench#readme |
| tinyexec | 1.3.0 | MIT | https://github.com/tinylibs/tinyexec#readme |
| tinyglobby | 0.2.17 | MIT | https://superchupu.dev/tinyglobby |
| tinyrainbow | 3.1.1 | MIT | https://github.com/tinylibs/tinyrainbow#readme |
| ts-api-utils | 2.5.0 | MIT | https://github.com/JoshuaKGoldberg/ts-api-utils#readme |
| type-check | 0.4.0 | MIT | https://github.com/gkz/type-check |
| typescript | 6.0.3 | Apache-2.0 | https://www.typescriptlang.org/ |
| typescript-eslint | 8.68.0 | MIT | https://typescript-eslint.io/packages/typescript-eslint |
| uri-js | 4.4.1 | BSD-2-Clause | https://github.com/garycourt/uri-js |
| util-deprecate | 1.0.2 | MIT | https://github.com/TooTallNate/util-deprecate |
| vite | 8.2.2 | MIT | https://vite.dev |
| vitefu | 1.1.3 | MIT | https://github.com/svitejs/vitefu#readme |
| vitest | 4.1.11 | MIT | https://vitest.dev |
| which | 2.0.2 | ISC | https://github.com/isaacs/node-which#readme |
| why-is-node-running | 2.3.0 | MIT | https://github.com/mafintosh/why-is-node-running |
| word-wrap | 1.2.5 | MIT | https://github.com/jonschlinkert/word-wrap |
| yaml | 1.10.3 | ISC | https://eemeli.org/yaml/v1/ |
| yocto-queue | 0.1.0 | MIT | https://github.com/sindresorhus/yocto-queue#readme |
| zimmerframe | 1.1.4 | MIT | https://github.com/sveltejs/zimmerframe#readme |
