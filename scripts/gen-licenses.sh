#!/usr/bin/env bash
# 依存ライセンス一覧の生成 (WP-0)。
#
# 使い方:
#   scripts/gen-licenses.sh          # docs/dev/licenses.md を生成
#
# 抽出対象:
#   - Rust workspace依存: cargo metadata (Cargo.lock解決グラフの全パッケージ)
#   - フロント依存:       pnpm licenses list (apps/desktop。prod+dev)
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

{
  echo "# 依存ライセンス一覧 (自動生成)"
  echo
  echo "- 生成日: $(date -u +%Y-%m-%d)"
  echo "- 生成コマンド: \`scripts/gen-licenses.sh\` (要 cargo / jq / pnpm)"
  echo "- 内容: Cargo.lock解決グラフの全Rustクレート + apps/desktopのnpm依存 (prod+dev)。"
  echo "  依存を追加・更新した際は再生成してコミットすること。"
  echo "- Rust側のworkspaceメンバー (gento-core/gento-image/gento-tauri) は本リポジトリ管理。"
  echo "  license欄が空のクレートはpackage.json/Cargo.tomlにlicense宣言がないもの"
  echo "  (入手元のリポジトリで確認すること)。"
  echo

  # ===== Rust (cargo metadata) =====
  echo "## Rust (workspace + Cargo.lock依存)"
  echo
  echo "| クレート | バージョン | license | 入手元 |"
  echo "|---|---|---|---|"
  # 解決グラフ (resolve.nodes) に現れるパッケージのみを列挙する。
  # path依存 (= workspaceメンバー) は「(workspace)」と表示する。
  cargo metadata --format-version 1 --manifest-path "$REPO_ROOT/Cargo.toml" \
    | jq -r '
      (.resolve.nodes[].id) as $ids
      | [.packages[] | select(.id as $p | $ids | index($p))]
      | sort_by(.name)
      | .[]
      | if (.source == null) then
          "| \(.name) | \(.version) | (workspace) | (このリポジトリ) |"
        else
          "| \(.name) | \(.version) | \(if .license then .license elif .license_file then "license_file: " + .license_file else "" end) | \(.repository // "https://crates.io/crates/" + .name) |"
        end'
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

echo "generated: $OUT"
