# Release Checklist (WP-18 — RC→公開の正本)

対象: GEN-TO MVP (計画書§26「WP-18: リリース候補」)。本書は
(a) **MVPリリース阻止条件7項目の証拠マトリクス**と (b) **RCビルド→検証→
公開の手順**の正本である。RC作成時に本書の§2・§3を最初から最後まで
実行し、結果を該当RCの検証記録に残す。

- 現在の状態 (2026-09-04): Linux実機E2Eは全て実測済み。
  **Windows実機・Wayland実機の検証は利用者承認済みでRCゲートへ先送り**
  (§4)。cargo基準 965 (9cf9b49時点963 + FG-WP18-1の2) / vitest基準
  549 (9cf9b49時点・49ファイル)。

## 1. MVPリリース阻止条件 — 証拠マトリクス

計画書§26「MVPリリース阻止条件」7項目と、各条件が**満たされていない**
(=リリース可能) ことを実証するテスト・コマンド・E2Eの対応表。
「日常CI」列は `cargo test --workspace` / `pnpm test` で常時実行される。

| # | 阻止条件 (成立したらリリース不可) | 証拠 (テスト/E2E/文書) | 日常CI |
|---|---|---|---|
| 1 | 元画像を意図せず変更または失くす | **publish系never-overwrite群** `apps/desktop/src-tauri/src/export.rs::tests`: `existing_conflict_no_silent_overwrite` / `manifest_never_replaces_even_gento_owned_file` / `manifest_never_overwrites_user_owned_file` / `publish_conflict_leaves_self_identifying_tmp_residue` / `publish_tmp_name_eexist_never_deletes_preexisting_entry`。**操作系** `crates/gento-core/src/file_ops.rs::tests`: `execute_copy_overwrite_onto_itself_refused` / `execute_trash_failure_keeps_everything_untouched` / `undo_refuses_when_file_modified_after_operation` ほか全undo群。**起動時sweep** (WP-17) `apps/desktop/src-tauri/src/fileops.rs::tests`: `sweep_keeps_unrecorded_strict_grammar_file` / `sweep_keeps_swapped_file_with_mismatched_ino` / `sweep_removes_recorded_tmp_with_matching_ino` / `recover_at_startup_sweeps_recorded_failed_step_residue` (record所有権+fd同一性・fail-closed残留。残存リスクは利用者承認済み — 直近コミットメッセージ参照) | 一部 (sweep・file_ops) |
| 2 | DB migrationでデータを失う | **全段階fixture実証 (WP-18-1)** `crates/gento-db/src/migrate.rs::tests`: `every_stage_fixture_upgrades_to_latest_with_data_retention` (v1〜v8各段階の代表データ投入DB→v9適用で適用成功+データ保持+backup作成+FK/integrity緑) / `stage8_fixture_is_noop_at_latest_and_retains_data`。既存網羅: `fresh_db_creates_latest_schema_and_sets_user_version` / `v2_database_upgrades_to_v4_with_backup` / `v3_database_upgrades_to_v4_with_backup` / `v7_populated_jobs_migrate_to_v8_with_legacy_and_duplicate_rows` / `failing_migration_rolls_back_and_keeps_backup` (途中失敗=rollback+backup保持) / `newer_schema_db_is_rejected` / `vacuum_backup_is_wal_safe_and_standalone`。crash復帰: `crash_simulation_wal_left_behind_recovers_on_reopen` (lib.rs) | ✅ |
| 3 | ローカルプロトコルから任意ファイルを読める | `apps/desktop/src-tauri/src/protocol.rs::tests` 監査群 (20件超): `rejects_non_localhost_authority` / `rejects_port_and_userinfo` / `rejects_percent_encoded_separators` / `rejects_backslash_and_dot_segments` / `rejects_wrong_segment_shape` / `rejects_invalid_ids` / `rejects_opaque_url` / `original_symlink_is_404` / `rejects_scheme_mismatch` ほか全reject系。サムネ/プレビュー/original以外の資源は返さない | ✅ |
| 4 | APIキー・GPS・画像内容が意図せず外部送信される | **鍵の非保存**: `crates/gento-ai/tests/pipeline_e2e.rs::e2e_api_key_sent_as_bearer_and_never_in_db` (Bearer送信のみ・DB不保存) / `crates/gento-ai/src/apikey.rs::tests` (`api_key_debug_and_display_are_redacted` / `symlinked_key_file_is_rejected_on_save_and_load` 等keyring不在時0600ファイルも検証)。**送信内容ゲート**: `crates/gento-ai/src/prompt.rs::tests` `privacy_is_and_of_root_and_profile` / `gps_is_omitted_unless_both_gates_on` (EXIF/GPSはルート設定ANDプロファイル設定のopt-in)。**ログ・診断**: `apps/desktop/src-tauri/src/ai.rs::tests::ai_failure_log_never_contains_api_key_or_gps_even_at_debug` / `apps/desktop/src-tauri/src/diagnostics.rs::tests::bundle_excludes_secrets_gps_and_real_paths`。**リダイレクト不追従** (意図しない転送先送信防止): `crates/gento-ai/src/client.rs` (`redirect::Policy::none()`。3xxはAPIエラー化) | ✅ |
| 5 | キャッシュまたはindex破損でアプリを起動できない | **サムネキャッシュ全削除→再生成 (WP-18-1)** `apps/desktop/src-tauri/src/thumbnails.rs::tests::deleted_cache_root_is_recreated_and_regenerates`。**ベクトルindex破損→自動再構築**: `apps/desktop/src-tauri/tests/vector_index_service.rs::build_search_restart_and_corruption_cycle` (SQLiteが正本 — indexは全再構築可能)。**FTS破損→検出・修復**: `crates/gento-db/src/search.rs::tests::integrity_detects_and_rebuild_repairs` + `fts_repair` コマンド (診断UI)。**DBが開けなくてもアプリ継続**: `apps/desktop/src-tauri/src/db.rs::tests::db_state_keeps_error_when_path_is_a_directory` / `db_state_future_schema_db_keeps_app_alive_with_recovery_guidance` (WP-18-1: 旧バイナリで新schema DBを開いた場合もpanicせず「新しいGEN-TOバイナリが必要」案内)。kill -9クラッシュ復帰: WP-16 S9 (`docs/dev/measurements/drivers/wp16_s9_kill.py`) | ✅ |
| 6 | AI endpoint不在で通常ファイラーが使えない | `apps/desktop/src-tauri/src/ai.rs::tests::service_e2e_endpoint_down_backoff_and_filer_unblocked` (endpointDOWNでバックオフしつつファイラー系は障害なし)。実機: 全X11 E2E (uxlib1・WP-16 S1〜S10・§3の復元演習) はAIプロファイル未設定で実施 | ✅ |
| 7 | WindowsまたはLinuxの主要フローが成立しない | **Linux=実測済み**: X11実機E2E群 (`docs/dev/measurements/drivers/uxlib1_e2e.sh`・`wp4_e2e_ui.sh`〜`wp9_e2e.sh`・WP-16 S1-S10 drivers・`scripts/wp18_backup_restore_drill.sh` 復元演習) + 性能基準 `docs/dev/measurements/wp16-baselines.md` + 配布物検証 (WP-17-2: deb/rpm/AppImageの起動確認・依存検査・sha256記録)。**Windows=先送り確定 (利用者承認済み)**: 再開条件は [platform-notes §8](platform-notes.md) の利用者手動検証チェックリスト実行 (NSIS/MSI bundle・WebView2・ゴミ箱・長いパス等)。Waylandも同様にRCゲートで検証 (X11でE2E実施済み) | E2Eは手動 |

各条件の証拠が不明瞭になった場合 (テスト名の変更・移動等) は
`cargo test --workspace` の件数増減と§3の検証コマンドで再確認する。

## 2. RCビルド手順

前提: 作業ツリーがクリーンであること (`git status`)。

```bash
# (1) フロントエンド
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop check   # svelte-check 0 error
pnpm --dir apps/desktop test    # vitest 全緑 (基準件数以上)
pnpm --dir apps/desktop lint    # eslint
pnpm --dir apps/desktop build   # vite build

# (2) Rust全軸
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p gento-tauri --all-targets --features devtools -- -D warnings
cargo test --workspace                          # 全緑 (基準件数以上)
cargo check --release -p gento-tauri --features custom-protocol,devtools

# (3) クロスコンパイル欠陥なし (darwin純Rust 3 crate — WP-17継続条件)
cargo check -p gento-core     --target aarch64-apple-darwin
cargo check -p gento-platform --target aarch64-apple-darwin
cargo check -p gento-search   --target aarch64-apple-darwin

# (4) bundle (Linux: deb/rpm/appimage — WP-17-2構成)
pnpm --dir apps/desktop tauri build

# (5) 成果物の記録 (sha256・サイズ)
cd target/release/bundle
sha256sum deb/*.deb rpm/*.rpm appimage/*.AppImage | tee <RC名>-sha256.txt
```

ビルド成果物はworkspace rootの `target/release/bundle/` 配下 (app別target
ではない)。

### devtools隔離の確認 (毎RC)

```bash
bash scripts/release-smoke.sh   # featureなしreleaseにdev系識別子不在+計測buildの動作
```

### 署名の現状 (2026-09-04)

- **Linux配布物は無署名** (deb/rpm/AppImageとも)。GPG鍵によるrepo署名は
  未実施 — 公開チャネルが決まった時点で追加する。
- **Windowsインストーラは自己署名 (無署名) をMVPでは許容** (利用者承認
  済み。SmartScreenは「詳細情報→実行」で迂回。platform-notes §8.1)。
  正式なcode signing証明書はMVP後の課題。

## 3. RC検証 (公開前の手動ゲート)

1. **§1のマトリクスを全行確認** — 該当テストが現行コードで緑であること
   (`cargo test --workspace` + テスト名指定で再実行)。
2. **実機E2E (Xephyr :6)**:
   - `bash docs/dev/measurements/drivers/uxlib1_e2e.sh` (主要フロー)
   - `bash scripts/wp18_backup_restore_drill.sh` (復元演習 — §1条件2/5/7の
     実機側証拠。終了後Xephyrをkill)
3. **パッケージ実機起動**: WP-17-2と同じ手順 — `dpkg-deb -x` 展開バイナリと
   AppImageをXディスプレイで起動・グリッド表示 (deb/rpm内容物同一性は
   `dpkg-deb -I/-c`・RPMヘッダ解析)。
4. **データ保持の再確認** (アップグレード・アンインストール):
   - アップグレード: 旧バージョンのDB (可能なら実運用DBのcopy) を新RCで
     開き §1条件2のテスト相当を確認 (migration backupが `db-backups/` に
     作成されること)。
   - アンインストール: パッケージ削除後も `~/.local/share/gento-spike/`
     (DB・設定・バックアップ) が残ること (Tauri/OS慣行 — READMEの
     「データの所在と寿命」参照)。デスクトップエントリ等の削除手順は
     `docs/handoff/2026-09-04-mvp-wp18-1.md` の手順書。
5. **ライセンス同梱**: `docs/dev/licenses.md` を再生成
   (`scripts/gen-licenses.sh`) し、`licenses/MODEL-LICENSES.md` (埋め込み
   モデル) とともに成果物へ同梱されていることを確認 (tauri.conf.json
   resources)。
   - 配布scope policy (FG-WP18-1): Rust側 = **Cargo.lockの全package**
     (build-tool系optional依存を含む)。完全性は `licenses_notice` unit
     test (gento-tauri) が `cargo test --workspace` で常時検証する。
     依存変更後の再生成を怠るとこのテストが赤くなる。
6. **バージョン情報**: `CHANGELOG.md`・about表示のバージョン/commitが
   正しいこと (WP-18後半の成果物)。

## 4. プラットフォーム条件

- **Linux (X11): 実測済み** — §1条件7のとおり。性能はwp16-baselines基準。
- **Linux (Wayland): 先送り (利用者承認済み)** — RCゲートで起動・主要
  フローを確認する (WebKitGTKはWayland対応、検証は未実施)。
- **Windows: 先送り (利用者承認済み)** — 再開条件 =
  [platform-notes §8](platform-notes.md)「Windows利用者手動検証
  チェックリスト」の実行結果がOKであること (§8.2主要フロー12項目 +
  §8.3 Windows特有5項目)。`bundle.targets` へ `nsis`/`msi` は設定済み
  (WP-17-1) だがWindows実機でのbundle生成・起動は未実施。
- **macOS: 正式対応外** (best-effort手順のみ platform-notes §9)。

## 5. 既知の先送り・申し送り項目 (公開判断の参照)

- sweep残存リスク (検証→unlink間の停止無限窓) — 利用者承認済み
  (WP-17レビュー記録)。
- Windows/Wayland実機検証 — §4のとおりRCゲートで実施。
- 全件生成の5万件スケール制限・WAL終了時残存・100MP表示方針 —
  WP-16申し送り (PM判断待ち。性能側でリリース阻止には非該当)。
