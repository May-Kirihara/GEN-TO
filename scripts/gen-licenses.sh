#!/usr/bin/env bash
# 依存ライセンス一覧の生成 (WP-0。FG-WP18-1でlockfile走査へ拡張)。
#
# 使い方:
#   scripts/gen-licenses.sh          # docs/dev/licenses.md を生成
#
# 配布scope policy (FG-WP18-1 — 最終レビューreject対策として恒久化):
#   Rust側の対象 = **Cargo.lockの全 [[package]]** (workspaceメンバー +
#   build-tool系を含む)。cargo metadataの解決graphはfeature/target構成で
#   変動し、optional依存 (cxx → cxxbridge-cmd → clap一族 等) が零れ得る
#   ため正本としない。license・入手元の情報源は cargo metadata とし、
#   metadataに現れないlockfile限定packageは下方の補完表 (crates.ioで
#   license/repositoryを確認済みのもの) から取る。Cargo.lockに存在して
#   metadataにも補完表にも無いpackageがある場合はエラーで停止する
#   (無掲載の放置を構造的に防ぐ)。
#   完全性の回帰検査は gento-tauri の licenses_notice unit test
#   (src/licenses_notice.rs) が「Cargo.lock全package掲載」を検証する
#   (cargo test --workspace で常時実行される)。
#
# フロント依存: pnpm licenses list (apps/desktop。prod+dev)
# 出力: docs/dev/licenses.md
#   クレート/パッケージ名・バージョン・license・入手元 (repository/homepage)
#
# 依存ツール: cargo, jq, pnpm
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OUT="$REPO_ROOT/docs/dev/licenses.md"
DESKTOP_DIR="$REPO_ROOT/apps/desktop"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
command -v pnpm >/dev/null || { echo "pnpm is required" >&2; exit 1; }

# ===== license/repository情報源 1: cargo metadata (解決graph内の外部package) =====
declare -A META_LICENSE META_REPO
while IFS=$'\t' read -r name version license repository; do
  META_LICENSE["$name $version"]="$license"
  META_REPO["$name $version"]="$repository"
done < <(cargo metadata --format-version 1 --manifest-path "$REPO_ROOT/Cargo.toml" \
  | jq -r '
    .packages[]
    | select(.source != null)
    | [.name, .version,
       (if .license then .license elif .license_file then "license_file: " + .license_file else "" end),
       (.repository // "https://crates.io/crates/" + .name)]
    | @tsv')

# ===== 補完表: metadata解決graph外のlockfile限定package (crates.io確認済み) =====
# 追加が必要になったら crates.io API で license/repository を確認して
# 「name version|license|repository」形式で追記すること。
SUPPLEMENT_TS=$'anstyle 1.0.14|MIT OR Apache-2.0|https://github.com/rust-cli/anstyle\nclap 4.6.6|MIT OR Apache-2.0|https://github.com/clap-rs/clap\nclap_builder 4.6.6|MIT OR Apache-2.0|https://github.com/clap-rs/clap\nclap_lex 1.1.0|MIT OR Apache-2.0|https://github.com/clap-rs/clap\ncxxbridge-cmd 1.0.199|MIT OR Apache-2.0|https://github.com/dtolnay/cxx'
declare -A SUPP_LICENSE SUPP_REPO
while IFS='|' read -r key license repository; do
  SUPP_LICENSE["$key"]="$license"
  SUPP_REPO["$key"]="$repository"
done <<< "$SUPPLEMENT_TS"

# ===== 正本のpackage一覧: Cargo.lock走査 (全 [[package]]) =====
# name/versionは引用符付き行のみ (lockfile先頭の version = 3 は無視)。
# source行の有無で workspaceメンバー (path) と外部package (ext) を区別。
lock_packages() {
  awk '
    /^\[\[package\]\]/ { name=""; ver=""; src=0; next }
    /^name = "/      { name = substr($0, 9, length($0) - 9) }
    /^version = "/   { ver = substr($0, 12, length($0) - 12) }
    /^source = /     { src=1 }
    /^$/             { if (name != "" && ver != "") print name "\t" ver "\t" (src ? "ext" : "path"); name=""; ver=""; src=0 }
    END              { if (name != "" && ver != "") print name "\t" ver "\t" (src ? "ext" : "path") }
  ' "$REPO_ROOT/Cargo.lock"
}

MISSING=0
{
  echo "# 依存ライセンス一覧 (自動生成)"
  echo
  echo "- 生成日: $(date -u +%Y-%m-%d)"
  echo "- 生成コマンド: \`scripts/gen-licenses.sh\` (要 cargo / jq / pnpm)"
  echo "- 内容: Rust = **Cargo.lockの全package** (workspaceメンバー + build-tool系"
  echo "  optional依存を含む。cargo metadata解決graph外の分も網羅 — FG-WP18-1)、"
  echo "  npm = apps/desktop依存 (prod+dev)。"
  echo "  依存を追加・更新した際は再生成してコミットすること (完全性は"
  echo "  gento-tauriの licenses_notice unit test が常時検証する)。"
  echo "- Rust側のworkspaceメンバー (gento-* の8 crate) は本リポジトリ管理。"
  echo "  license欄が空のクレートはpackage.json/Cargo.tomlにlicense宣言がないもの"
  echo "  (入手元のリポジトリで確認すること)。"
  echo

  # ===== Rust (Cargo.lock全package) =====
  echo "## Rust (Cargo.lock全package)"
  echo
  echo "| クレート | バージョン | license | 入手元 |"
  echo "|---|---|---|---|"
  while IFS=$'\t' read -r name ver kind; do
    if [ "$kind" = "path" ]; then
      printf '| %s | %s | (workspace) | (このリポジトリ) |\n' "$name" "$ver"
      continue
    fi
    key="$name $ver"
    license="${META_LICENSE[$key]-${SUPP_LICENSE[$key]-}}"
    repository="${META_REPO[$key]-${SUPP_REPO[$key]-}}"
    if [ -z "$license" ] && [ -z "$repository" ]; then
      echo "gen-licenses.sh: Cargo.lockのpackageがcargo metadataにも補完表にも無い: $key" >&2
      echo "  → crates.ioでlicense/repositoryを確認し、スクリプト内の補完表へ追加すること" >&2
      MISSING=1
      continue
    fi
    printf '| %s | %s | %s | %s |\n' "$name" "$ver" "$license" "$repository"
  done < <(lock_packages | LC_ALL=C sort -t$'\t' -k1,1 -k2,2)
  echo

  # ===== TypeScript / Svelte (pnpm licenses list) =====
  echo "## TypeScript / Svelte (apps/desktop)"
  echo
  echo "| パッケージ | バージョン | license | 入手元 |"
  echo "|---|---|---|---|"
  (cd "$DESKTOP_DIR" && pnpm licenses list --json) \
    | jq -r '
      to_entries
      | map(.value[])
      | sort_by(.name)
      | .[]
      | "| \(.name) | \(.versions | join(", ")) | \(.license) | \(.homepage // "") |"'
} > "$OUT"

if [ "$MISSING" -ne 0 ]; then
  rm -f "$OUT"
  echo "error: 補完表が不足しているため生成を中止した (上記ログ参照)" >&2
  exit 1
fi

echo "generated: $OUT"
