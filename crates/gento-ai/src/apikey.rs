//! APIキーの安全な取り扱い (MVP WP-10。spec §16)。
//!
//! - ApiKey: 鍵のnewtype。`Debug`/`Display`は常に行を伏せる
//!   (マクロ経由のログ・panic出力への意図しない漏出を構造的に防ぐ)。
//! - SecretStore: OS資格情報ストア (keyring crate。Linuxでは
//!   Secret Service) への保存を第一とし、実行環境で利用不能な場合のみ
//!   アプリデータ_dir直下の専用ファイル (0600・平文・警告同梱) へ
//!   フォールバックする。**SQLiteと通常ログへ鍵を書く経路は存在しない**
//!   (acceptance 4。検証: pipeline tests / api_keys_never_in_db)。
//!
//! フォールバックを「簡易難読化」にしない理由: 暗号化の鍵を同一
//! マシン上に置く疑似安全は、安全性の実態を曖昧にするだけのため。
//! 平文であることを警告で明示する (filesの隣にREADMEを置く)。

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

/// APIキー (秘密値)。Debug/Displayは伏せ字。
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: String) -> Self {
        ApiKey(value)
    }

    /// 鍵本体 (Authorizationヘッダ構築など、鍵を使う側でのみ呼ぶ)。
    pub fn secret(&self) -> &str {
        &self.0
    }
}

// 伏せ字: ログ・エラー文字列への混入を構造的に防ぐ。
impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(****)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("****")
    }
}

/// 保存先の種別 (UI表示・警告用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStoreKind {
    /// OS資格情報ストア (Linux: Secret Service / macOS: Keychain /
    /// Windows: Credential Manager)。
    Keyring,
    /// アプリデータ_dir直下の平文ファイル (0600)。Secret Serviceが
    /// 利用不能な環境のフォールバック。
    PlainFile,
}

impl SecretStoreKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            SecretStoreKind::Keyring => "keyring",
            SecretStoreKind::PlainFile => "file",
        }
    }
}

/// 資格情報ストア参照ID (ai_profiles.api_key_ref へ保存する文字列)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSecretRef {
    pub kind: SecretStoreKind,
    /// keyring: エントリのユーザー名部分 / file: ファイル名。
    pub id: String,
}

impl StoredSecretRef {
    /// `ai_profiles.api_key_ref` 形式 ("keyring:<id>" / "file:<id>") へ。
    pub fn as_ref_string(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.id)
    }

    /// 参照文字列をパースする。不正形式はNone。
    pub fn parse(value: &str) -> Option<Self> {
        let (kind, id) = value.split_once(':')?;
        let kind = match kind {
            "keyring" => SecretStoreKind::Keyring,
            "file" => SecretStoreKind::PlainFile,
            _ => return None,
        };
        if id.is_empty() {
            return None;
        }
        Some(StoredSecretRef {
            kind,
            id: id.to_string(),
        })
    }
}

/// keyringのサービス名 (Secret Serviceのコレクション内での識別)。
const KEYRING_SERVICE: &str = "gento";

/// フォールバックファイルのディレクトリ名 (app data_dir直下)。
pub const SECRET_DIR_NAME: &str = "ai-secrets";

/// フォールバック先に必ず置く警告文のファイル名。
const README_NAME: &str = "_README.txt";

const README_TEXT: &str = "\
このディレクトリには GEN-TO のAI接続プロファイルのAPIキーが
「平文」で保存されています (OSの資格情報ストア = Secret Service が
利用できない環境のためのフォールバックです)。

- ファイルは所有者のみ読み書き可能 (0600) になっています
- ディレクトリごとバックアップ・同期する場合は注意してください
- 資格情報ストアが利用できる環境では再保存することで移行できます
";

/// パスが存在する場合、symlinkでないことを保証する (差し替え攻撃の
/// 防衛 — 既存symlinkは失敗扱い。リンクを辿らないようsymlink_metadata)。
fn ensure_not_symlink(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(std::io::Error::other(format!(
            "refusing symlinked secret path: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// 鍵の保存・読み出し。
pub struct SecretStore {
    /// フォールバックファイルの置き場所 (app data_dir直下のai-secrets)。
    secret_dir: PathBuf,
    /// テスト用: keyringを試行せず常にファイルへ保存する。
    force_file: bool,
}

#[derive(Debug)]
pub enum SecretStoreError {
    /// 両方の保存先が失敗した (keyring不可 + ファイルIOエラー)。
    BothFailed {
        keyring: String,
        file: String,
    },
    Io(String),
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretStoreError::BothFailed { keyring, file } => write!(
                f,
                "keyring save failed ({keyring}) and plain-file fallback failed ({file})"
            ),
            SecretStoreError::Io(msg) => write!(f, "secret store io error: {msg}"),
        }
    }
}

impl std::error::Error for SecretStoreError {}

impl SecretStore {
    pub fn new(data_dir: &Path) -> Self {
        SecretStore {
            secret_dir: data_dir.join(SECRET_DIR_NAME),
            force_file: false,
        }
    }

    /// テスト用コンストラクタ: keyringを経由しない。
    pub fn file_only(data_dir: &Path) -> Self {
        SecretStore {
            secret_dir: data_dir.join(SECRET_DIR_NAME),
            force_file: true,
        }
    }

    pub fn secret_dir(&self) -> &Path {
        &self.secret_dir
    }

    /// 鍵を保存し、参照IDを返す。
    ///
    /// 1. keyring (Secret Service) を試行する
    /// 2. 失敗 (D-Bus不在・ロック等) はファイルフォールバックへ
    ///    (0600・README警告を併設)
    pub fn save(
        &self,
        profile_uuid: &str,
        key: &ApiKey,
    ) -> Result<StoredSecretRef, SecretStoreError> {
        if !self.force_file {
            match keyring::Entry::new(KEYRING_SERVICE, profile_uuid) {
                Ok(entry) => match entry.set_password(key.secret()) {
                    Ok(()) => {
                        return Ok(StoredSecretRef {
                            kind: SecretStoreKind::Keyring,
                            id: profile_uuid.to_string(),
                        })
                    }
                    Err(err) => {
                        // 鍵本体を含まないエラー文のみログ化
                        tracing::debug!(error = %err, "keyring unavailable; falling back to file");
                    }
                },
                Err(err) => {
                    tracing::debug!(error = %err, "keyring entry creation failed; falling back to file");
                }
            }
        }
        self.save_to_file(profile_uuid, key)
    }

    fn save_to_file(
        &self,
        profile_uuid: &str,
        key: &ApiKey,
    ) -> Result<StoredSecretRef, SecretStoreError> {
        let save = || -> std::io::Result<()> {
            // ディレクトリ自体がsymlink (差し替え攻撃) は保存失敗扱い
            if self.secret_dir.symlink_metadata().is_ok() {
                ensure_not_symlink(&self.secret_dir)?;
            }
            std::fs::create_dir_all(&self.secret_dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                // 平文置き場のため所有者限定 (0700)。既存ディレクトリの
                // モードも保存のたびに矯正する。
                let meta = std::fs::metadata(&self.secret_dir)?;
                let mut perms = meta.permissions();
                perms.set_mode(0o700);
                std::fs::set_permissions(&self.secret_dir, perms)?;
                // 平文であるため所有者限定 (0600)。既存ファイルのモードも
                // 書き直し時に併せて落とす (create_newではなくtruncateで上書き)。
                let file_name = self.file_name_of(profile_uuid);
                if file_name.symlink_metadata().is_ok() {
                    ensure_not_symlink(&file_name)?;
                    let meta = std::fs::metadata(&file_name)?;
                    let mut perms = meta.permissions();
                    perms.set_mode(0o600);
                    std::fs::set_permissions(&file_name, perms)?;
                }
                std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .create(true)
                    .mode(0o600)
                    .open(&file_name)?
                    .write_all(key.secret().as_bytes())?;
            }
            #[cfg(not(unix))]
            {
                // Windows/macOS: keyringが実質常に利用可能なため、
                // フォールバックは到達しにくい。最小限のACL無し書き込み。
                // (ACL付与はWP-17で検証する — handoff参照)
                let file_name = self.file_name_of(profile_uuid);
                if file_name.symlink_metadata().is_ok() {
                    ensure_not_symlink(&file_name)?;
                }
                std::fs::write(&file_name, key.secret().as_bytes())?;
            }
            let readme = self.secret_dir.join(README_NAME);
            if !readme.exists() {
                std::fs::write(&readme, README_TEXT)?;
            }
            Ok(())
        };
        save().map_err(|e| SecretStoreError::Io(e.to_string()))?;
        Ok(StoredSecretRef {
            kind: SecretStoreKind::PlainFile,
            id: profile_uuid.to_string(),
        })
    }

    fn file_name_of(&self, profile_uuid: &str) -> PathBuf {
        // uuidは36文字の固定形式 (ai_profilesのCHECK) のためパス注入なし。
        self.secret_dir.join(format!("{profile_uuid}.key"))
    }

    /// 参照IDから鍵を読む。未登録・読み取り失敗はNone
    /// (APIキー無しendpoint = ローカルllama.cpp等は正当な状態)。
    /// **鍵ファイル/ディレクトリがsymlinkの場合もNone** — リンク先の
    /// 内容を鍵として読み上げない (フェイルクローズ)。
    pub fn load(&self, api_key_ref: &str) -> Option<ApiKey> {
        let secret_ref = StoredSecretRef::parse(api_key_ref)?;
        match secret_ref.kind {
            SecretStoreKind::Keyring => {
                let entry = keyring::Entry::new(KEYRING_SERVICE, &secret_ref.id).ok()?;
                entry.get_password().ok().map(ApiKey::new)
            }
            SecretStoreKind::PlainFile => {
                if ensure_not_symlink(&self.secret_dir).is_err() {
                    return None;
                }
                let path = self.file_name_of(&secret_ref.id);
                if ensure_not_symlink(&path).is_err() {
                    return None;
                }
                std::fs::read_to_string(path)
                    .ok()
                    .map(|s| ApiKey::new(s.trim_end_matches(['\n', '\r']).to_string()))
            }
        }
    }

    /// 参照先の鍵を削除する (プロファイル削除時。失敗は無視 —
    /// 残っても参照IDを失うため実質無害)。
    pub fn delete(&self, api_key_ref: &str) {
        let Some(secret_ref) = StoredSecretRef::parse(api_key_ref) else {
            return;
        };
        match secret_ref.kind {
            SecretStoreKind::Keyring => {
                if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &secret_ref.id) {
                    let _ = entry.delete_credential();
                }
            }
            SecretStoreKind::PlainFile => {
                let _ = std::fs::remove_file(self.file_name_of(&secret_ref.id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-ai-apikey-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn api_key_debug_and_display_are_redacted() {
        let key = ApiKey::new("sk-super-secret-000".to_string());
        assert_eq!(format!("{key:?}"), "ApiKey(****)");
        assert_eq!(format!("{key}"), "****");
        assert_eq!(format!("{key:?}"), "ApiKey(****)");
        // PartialEqは本体比較 (実利用側の一致性検証のため)
        assert_eq!(key, ApiKey::new("sk-super-secret-000".to_string()));
    }

    #[test]
    fn secret_ref_roundtrip() {
        let r = StoredSecretRef {
            kind: SecretStoreKind::Keyring,
            id: "01912345-6789-7abc-8000-000000000001".to_string(),
        };
        let s = r.as_ref_string();
        assert_eq!(s, "keyring:01912345-6789-7abc-8000-000000000001");
        assert_eq!(StoredSecretRef::parse(&s).unwrap(), r);
        assert!(StoredSecretRef::parse("file:abc").is_some());
        assert!(StoredSecretRef::parse("bogus").is_none());
        assert!(StoredSecretRef::parse("file:").is_none());
        assert!(StoredSecretRef::parse("").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn file_fallback_writes_0600_and_warning() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("file");
        let store = SecretStore::file_only(tmp.0.as_path());
        let uuid = "019abcde-f012-7abc-8000-00000000000f";
        let stored = store
            .save(uuid, &ApiKey::new("sk-file-test".to_string()))
            .unwrap();
        assert_eq!(stored.kind, SecretStoreKind::PlainFile);
        let file = store.secret_dir().join(format!("{uuid}.key"));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "所有者のみ読み書き可能");
        assert!(store.secret_dir().join(README_NAME).exists(), "警告文同梱");
        // 読み出し往復 (末尾改行は許容)
        assert_eq!(
            store.load(&stored.as_ref_string()).unwrap().secret(),
            "sk-file-test"
        );
        // 削除
        store.delete(&stored.as_ref_string());
        assert!(store.load(&stored.as_ref_string()).is_none());
    }

    #[test]
    fn file_fallback_overwrite_keeps_0600() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = TempDir::new("overwrite");
            let store = SecretStore::file_only(tmp.0.as_path());
            let uuid = "019abcde-f012-7abc-8000-000000000010";
            store.save(uuid, &ApiKey::new("first".to_string())).unwrap();
            store
                .save(uuid, &ApiKey::new("second-value".to_string()))
                .unwrap();
            let ref_str = StoredSecretRef {
                kind: SecretStoreKind::PlainFile,
                id: uuid.to_string(),
            }
            .as_ref_string();
            assert_eq!(store.load(&ref_str).unwrap().secret(), "second-value");
            let file = store.secret_dir().join(format!("{uuid}.key"));
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_fallback_dir_is_0700() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("dirmode");
        let store = SecretStore::file_only(tmp.0.as_path());
        let uuid = "019abcde-f012-7abc-8000-000000000020";
        store
            .save(uuid, &ApiKey::new("sk-dir-mode".to_string()))
            .unwrap();
        let mode = std::fs::metadata(store.secret_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "ai-secretsは所有者のみアクセス可");
        // 緩められた既存ディレクトリも保存時に矯正される
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(store.secret_dir()).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(store.secret_dir(), perms).unwrap();
        }
        store
            .save(uuid, &ApiKey::new("sk-again".to_string()))
            .unwrap();
        let mode = std::fs::metadata(store.secret_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "既存dirのモードも矯正");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_key_file_is_rejected_on_save_and_load() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new("symlink");
        let store = SecretStore::file_only(tmp.0.as_path());
        let uuid = "019abcde-f012-7abc-8000-000000000021";
        // 一度正当に保存
        let stored = store
            .save(uuid, &ApiKey::new("sk-real".to_string()))
            .unwrap();
        // 鍵ファイルを外部ファイルへのsymlinkへ差し替え
        let outside = tmp.0.join("outside.txt");
        std::fs::write(&outside, "sk-attacker").unwrap();
        let key_file = store.secret_dir().join(format!("{uuid}.key"));
        std::fs::remove_file(&key_file).unwrap();
        symlink(&outside, &key_file).unwrap();
        // 上書き保存は失敗 (symlinkを経由した書き込みを拒否)
        assert!(store
            .save(uuid, &ApiKey::new("sk-new".to_string()))
            .is_err());
        // 読み出しも失敗 (リンク先の内容を読み上げない)
        assert!(store.load(&stored.as_ref_string()).is_none());
        // 攻撃者ファイルは無傷 (書き込まれていない)
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "sk-attacker",
            "symlink先へ書き込んでいない"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_secret_dir_is_rejected() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new("symdir");
        // ai-secrets自体を別ディレクトリへのsymlinkにする
        let real_dir = tmp.0.join("elsewhere");
        std::fs::create_dir_all(&real_dir).unwrap();
        symlink(&real_dir, tmp.0.join(SECRET_DIR_NAME)).unwrap();
        let store = SecretStore::file_only(tmp.0.as_path());
        let uuid = "019abcde-f012-7abc-8000-000000000022";
        let result = store.save(uuid, &ApiKey::new("sk-dir-link".to_string()));
        assert!(result.is_err(), "symlinkディレクトリへの保存は失敗");
        // リンク先へファイルが書かれていない
        assert!(
            std::fs::read_dir(&real_dir).unwrap().count() == 0,
            "リンク先は空のまま"
        );
    }

    #[test]
    fn keyring_save_attempts_and_reports_fallback() {
        // 実環境のkeyringを試行する (ヘッドレスでは失敗→ファイルへ
        // フォールバックする。どちらに転んでも参照が得られ鍵が読める)。
        let tmp = TempDir::new("keyring");
        let store = SecretStore::new(tmp.0.as_path());
        let uuid = "019abcde-f012-7abc-8000-000000000011";
        let stored = store
            .save(uuid, &ApiKey::new("sk-either".to_string()))
            .unwrap();
        match stored.kind {
            SecretStoreKind::Keyring => {
                // Secret Serviceが生きていた環境
                assert_eq!(
                    store.load(&stored.as_ref_string()).unwrap().secret(),
                    "sk-either"
                );
                store.delete(&stored.as_ref_string());
            }
            SecretStoreKind::PlainFile => {
                assert_eq!(
                    store.load(&stored.as_ref_string()).unwrap().secret(),
                    "sk-either"
                );
            }
        }
    }
}
