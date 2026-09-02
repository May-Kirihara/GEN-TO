//! PNG生成メタデータの解析 (WP-9): SD WebUI/Forge parameters・ComfyUI・NovelAI。

use gento_core::assetmeta::{AssetMetadataParsed, MetadataColumns};

use super::png_chunks::PngTextChunks;
use super::{PARSER_COMFY, PARSER_NAI, PARSER_SD_PARAMS};

/// テキストチャンク群から生成系parser (sd-params / comfy / nai) を適用する。
/// 該当なしは空Vec。
pub(crate) fn parse_generation_chunks(chunks: &PngTextChunks) -> Vec<AssetMetadataParsed> {
    let mut out = Vec::new();
    if let Some(row) = parse_sd_parameters(chunks) {
        out.push(row);
    }
    if let Some(row) = parse_comfy(chunks) {
        out.push(row);
    }
    if let Some(row) = parse_novelai(chunks) {
        out.push(row);
    }
    out
}

fn text_of<'a>(texts: &'a [(String, String)], keyword: &str) -> Option<&'a str> {
    texts
        .iter()
        .find(|(k, _)| k == keyword)
        .map(|(_, v)| v.as_str())
}

// ===== SD WebUI / Forge parameters =====

/// `parameters` チャンクのテキスト形式を解析する。
/// 形式: `<prompt行群>\nNegative prompt: <neg行群>\nSteps: 20, Sampler: …`
/// `Steps:` 行が存在しない場合は非検出 (None)。
pub(crate) fn parse_sd_parameters(chunks: &PngTextChunks) -> Option<AssetMetadataParsed> {
    let text = text_of(&chunks.texts, "parameters")?;
    let lines: Vec<&str> = text.split('\n').collect();
    // params行 = "Steps:" で始まる行 (標準では最終行)
    let params_index = lines.iter().position(|l| l.starts_with("Steps:"))?;
    // negative行 = "Negative prompt: " で始まる行 (promptとparamsの間)
    let neg_index = lines[..params_index]
        .iter()
        .position(|l| l.starts_with("Negative prompt:"));

    let (positive, negative) = match neg_index {
        Some(neg) => {
            let neg_body = lines[neg..params_index].join("\n");
            let neg_text = neg_body
                .strip_prefix("Negative prompt: ")
                .unwrap_or(&neg_body)
                .trim()
                .to_string();
            (
                lines[..neg].join("\n").trim().to_string(),
                (!neg_text.is_empty()).then_some(neg_text),
            )
        }
        None => (lines[..params_index].join("\n").trim().to_string(), None),
    };

    let mut columns = MetadataColumns {
        generator: Some("Stable Diffusion WebUI".to_string()),
        positive_prompt: (!positive.is_empty()).then_some(positive),
        negative_prompt: negative
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(|n| n.to_string()),
        ..MetadataColumns::default()
    };
    let mut extra: Vec<(String, serde_json::Value)> = Vec::new();
    let mut loras: Vec<serde_json::Value> = Vec::new();

    for (key, value) in split_params_line(lines[params_index]) {
        match key.as_str() {
            "Steps" => columns.steps = value.parse::<i64>().ok(),
            "Sampler" => columns.sampler = non_empty(&value),
            "Schedule type" => columns.scheduler = non_empty(&value),
            "CFG scale" => columns.cfg_scale = value.parse::<f64>().ok(),
            "Seed" => columns.seed = non_empty(&value),
            "Model" => columns.generation_model = non_empty(&value),
            "VAE" => columns.vae = non_empty(&value),
            "Size" => extra.push(("size".into(), serde_json::json!(value))),
            "Model hash" => extra.push(("model_hash".into(), serde_json::json!(value))),
            "Clip skip" => extra.push((
                "clip_skip".into(),
                serde_json::json!(value.parse::<i64>().unwrap_or(0)),
            )),
            "Version" => extra.push(("version".into(), serde_json::json!(value))),
            "Denoising strength" => extra.push((
                "denoise".into(),
                serde_json::json!(value.parse::<f64>().unwrap_or(0.0)),
            )),
            "Lora hashes" => {
                // "name1:hash1, name2:hash2" (全体が引用符で括られることがある)
                // → loras配列 (検索用)
                let trimmed = value.trim_matches('"');
                for pair in trimmed.split(", ") {
                    let pair = pair.trim_matches('"');
                    if let Some((name, hash)) = pair.split_once(':') {
                        loras.push(serde_json::json!({
                            "name": name.trim(),
                            "model_hash": hash.trim(),
                        }));
                    }
                }
                extra.push(("lora_hashes".into(), serde_json::json!(value)));
            }
            "TI hashes" => extra.push(("ti_hashes".into(), serde_json::json!(value))),
            _ => extra.push((key, serde_json::json!(value))),
        }
    }
    if !loras.is_empty() {
        extra.push(("loras".into(), serde_json::Value::Array(loras)));
    }
    if !extra.is_empty() {
        extra.sort_by(|a, b| a.0.cmp(&b.0));
        let map = serde_json::Map::from_iter(extra);
        columns.generation_extra =
            Some(serde_json::to_string(&serde_json::Value::Object(map)).unwrap());
    }
    Some(AssetMetadataParsed {
        parser_name: PARSER_SD_PARAMS.to_string(),
        parser_version: "1".to_string(),
        columns,
        raw_exif: None,
        raw_iptc: None,
        raw_png_metadata: None, // parameters生テキストはpng-text行のrawに含まれる
        raw_workflow: None,
    })
}

/// params行 "Steps: 20, Sampler: Euler a, …" を (key, value) 列へ分解する。
/// 二重引用符内の ", " は区切りとみなさない (Lora hashes等の引用値対策)。
/// さらに `:`を含まない断片は直前の値へ連結する (値内カンマ対策)。
fn split_params_line(line: &str) -> Vec<(String, String)> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ',' if !in_quotes && chars.peek() == Some(&' ') => {
                chars.next(); // 空白を消費
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for part in parts {
        match part.split_once(':') {
            Some((key, value)) if !key.is_empty() => {
                out.push((key.trim().to_string(), value.trim().to_string()));
            }
            _ => {
                // カンマを含む値の続き — 直前の値へ戻す
                if let Some(last) = out.last_mut() {
                    last.1.push_str(", ");
                    last.1.push_str(&part);
                }
            }
        }
    }
    out
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

// ===== ComfyUI =====

/// `prompt` (API形式JSONノードグラフ) から生成条件を抽出し、`workflow`
/// チャンクをraw_workflowへ保持する。`prompt`がJSONとして不正なら非検出。
pub(crate) fn parse_comfy(chunks: &PngTextChunks) -> Option<AssetMetadataParsed> {
    let prompt_text = text_of(&chunks.texts, "prompt")?;
    let graph: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(prompt_text).ok()?;
    if graph.is_empty() {
        return None;
    }

    let mut columns = MetadataColumns {
        generator: Some("ComfyUI".to_string()),
        ..MetadataColumns::default()
    };
    // KSampler系ノード (ノードID順に最初の1つを採用)
    let mut sampler_ids: Vec<&String> = graph
        .keys()
        .filter(|id| {
            graph[*id]
                .get("class_type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t == "KSampler" || t == "KSamplerAdvanced")
        })
        .collect();
    sampler_ids.sort_by_key(|id| id.parse::<u64>().unwrap_or(u64::MAX));
    if let Some(id) = sampler_ids.first() {
        let inputs = graph[*id].get("inputs");
        let get_str = |key: &str| {
            inputs
                .and_then(|i| i.get(key))
                .and_then(value_as_plain_string)
        };
        let get_num = |key: &str| {
            inputs
                .and_then(|i| i.get(key))
                .and_then(|v| v.as_f64())
                .map(|f| f as i64)
        };
        let get_float = |key: &str| inputs.and_then(|i| i.get(key)).and_then(|v| v.as_f64());
        columns.steps = get_num("steps");
        columns.cfg_scale = get_float("cfg");
        columns.sampler = get_str("sampler_name");
        columns.scheduler = get_str("scheduler");
        // KSampler: seed / KSamplerAdvanced: noise_seed
        columns.seed = get_str("seed").or_else(|| get_str("noise_seed"));
        columns.positive_prompt = resolve_conditioning_text(&graph, inputs, "positive");
        columns.negative_prompt = resolve_conditioning_text(&graph, inputs, "negative");
    }
    // ローダー系
    for node in graph.values() {
        let class = node
            .get("class_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let key = match class {
            "CheckpointLoaderSimple" | "CheckpointLoader" => "ckpt_name",
            "VAELoader" => "vae_name",
            "LoraLoader" => "lora_name",
            _ => continue,
        };
        let Some(name) = node
            .get("inputs")
            .and_then(|i| i.get(key))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        match class {
            "CheckpointLoaderSimple" | "CheckpointLoader" => {
                if columns.generation_model.is_none() {
                    columns.generation_model = Some(name.to_string());
                }
            }
            "VAELoader" if columns.vae.is_none() => {
                columns.vae = Some(name.to_string());
            }
            _ => {}
        }
    }
    // LoraLoader収集 → generation_extra.loras
    let mut loras: Vec<serde_json::Value> = Vec::new();
    for node in graph.values() {
        if node.get("class_type").and_then(|v| v.as_str()) != Some("LoraLoader") {
            continue;
        }
        let inputs = node.get("inputs");
        let name = inputs
            .and_then(|i| i.get("lora_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let strength = inputs
            .and_then(|i| i.get("strength_model"))
            .and_then(|v| v.as_f64());
        if !name.is_empty() {
            loras.push(serde_json::json!({"name": name, "strength_model": strength}));
        }
    }
    let mut extra = serde_json::Map::new();
    if !loras.is_empty() {
        extra.insert("loras".into(), serde_json::Value::Array(loras));
    }
    if !extra.is_empty() {
        columns.generation_extra =
            Some(serde_json::to_string(&serde_json::Value::Object(extra)).unwrap());
    }

    let workflow = text_of(&chunks.texts, "workflow").map(|w| w.to_string());
    Some(AssetMetadataParsed {
        parser_name: PARSER_COMFY.to_string(),
        parser_version: "1".to_string(),
        columns,
        raw_exif: None,
        raw_iptc: None,
        raw_png_metadata: None,
        raw_workflow: workflow,
    })
}

/// JSON値をシンプルな文字列へ (数値は正確な桁のまま文字列化 — Seed保全)。
fn value_as_plain_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// KSamplerのpositive/negative入力からプロンプトテキストを抽出する。
/// 入力は `[node_id, slot]` (link参照) — 参照先がCLIPTextEncodeなら
/// `inputs.text`、他の条件操作ノード (ConditioningCombine等) ならその
/// link入力を再帰的に追う (ベストエフォート。解決不能ならNone)。
fn resolve_conditioning_text(
    graph: &serde_json::Map<String, serde_json::Value>,
    inputs: Option<&serde_json::Value>,
    key: &str,
) -> Option<String> {
    let entry = inputs?.get(key)?;
    // 直接文字列 (まれな直接埋め込み)
    if let Some(text) = entry.as_str() {
        return (!text.trim().is_empty()).then(|| text.to_string());
    }
    let mut collected: Vec<String> = Vec::new();
    let mut queue: Vec<String> = Vec::new();
    if let Some(id) = link_target(entry) {
        queue.push(id);
    }
    let mut depth = 0;
    while let Some(id) = queue.pop() {
        depth += 1;
        if depth > 64 {
            break; // 循環・異常グラフの保護
        }
        let Some(node) = graph.get(&id) else {
            continue;
        };
        let class = node
            .get("class_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let node_inputs = node.get("inputs");
        match class {
            "CLIPTextEncode" => {
                if let Some(text) = node_inputs
                    .and_then(|i| i.get("text"))
                    .and_then(|v| v.as_str())
                {
                    if !text.trim().is_empty() {
                        collected.push(text.to_string());
                    }
                }
            }
            // 条件操作ノード: link入力をさらに追う
            "ConditioningCombine"
            | "ConditioningConcat"
            | "ConditioningAverage"
            | "ConditioningConcat3"
            | "ConditioningCombine3"
            | "ConditioningSetMask"
            | "PromptToConditioning" => {
                if let Some(obj) = node_inputs.and_then(|v| v.as_object()) {
                    for value in obj.values() {
                        if let Some(next) = link_target(value) {
                            queue.push(next);
                        }
                    }
                }
            }
            _ => {} // 未知ノードは追わない (unknown扱い)
        }
    }
    if collected.is_empty() {
        return None;
    }
    // キュー逆順 (pop) で走査したため、ファイル順へ戻す
    collected.reverse();
    Some(collected.join("\n"))
}

/// `[node_id, slot]` 形式のlink参照からnode_idを取り出す。
fn link_target(value: &serde_json::Value) -> Option<String> {
    let arr = value.as_array()?;
    if arr.is_empty() {
        return None;
    }
    match &arr[0] {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

// ===== NovelAI =====

/// NAI形式 (`Comment` JSON + `Title`/`Description`/`Software`/`Source`) を
/// 解析する。検出条件: `Software` == "NovelAI" または `Comment` が
/// `prompt` キーを持つJSON。
pub(crate) fn parse_novelai(chunks: &PngTextChunks) -> Option<AssetMetadataParsed> {
    let software = text_of(&chunks.texts, "Software");
    let comment_text = text_of(&chunks.texts, "Comment");
    let comment: Option<serde_json::Map<String, serde_json::Value>> =
        comment_text.and_then(|t| serde_json::from_str(t).ok());
    let is_nai =
        software == Some("NovelAI") || comment.as_ref().is_some_and(|c| c.contains_key("prompt"));
    if !is_nai {
        return None;
    }
    let comment = comment?;
    let mut columns = MetadataColumns {
        generator: Some("NovelAI".to_string()),
        ..MetadataColumns::default()
    };
    let get_str = |key: &str| {
        comment
            .get(key)
            .and_then(value_as_plain_string)
            .and_then(|s| non_empty(&s))
    };
    // positive: Descriptionが正本 (Comment.promptは同一内容)
    columns.positive_prompt = text_of(&chunks.texts, "Description")
        .and_then(non_empty)
        .or_else(|| get_str("prompt"));
    columns.negative_prompt = get_str("uc");
    columns.steps = comment.get("steps").and_then(|v| v.as_i64());
    columns.cfg_scale = comment.get("scale").and_then(|v| v.as_f64());
    columns.seed = get_str("seed");
    columns.sampler = get_str("sampler");
    columns.generation_model = text_of(&chunks.texts, "Source")
        .and_then(non_empty)
        .or_else(|| get_str("model"));
    // 残りの設定 (サイズ等) はgeneration_extraへ
    let mut extra = serde_json::Map::new();
    for (key, value) in &comment {
        if !matches!(
            key.as_str(),
            "prompt" | "uc" | "steps" | "scale" | "seed" | "sampler" | "model"
        ) {
            extra.insert(key.clone(), value.clone());
        }
    }
    if !extra.is_empty() {
        columns.generation_extra =
            Some(serde_json::to_string(&serde_json::Value::Object(extra)).unwrap());
    }
    Some(AssetMetadataParsed {
        parser_name: PARSER_NAI.to_string(),
        parser_version: "1".to_string(),
        columns,
        raw_exif: None,
        raw_iptc: None,
        raw_png_metadata: None,
        raw_workflow: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks_with(texts: &[(&str, &str)]) -> PngTextChunks {
        PngTextChunks {
            texts: texts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            exif: None,
            warnings: Vec::new(),
        }
    }

    // ===== SD parameters =====

    #[test]
    fn sd_params_full_form() {
        let chunks = chunks_with(&[(
            "parameters",
            "masterpiece, best quality, 1girl\nsecond prompt line\nNegative prompt: lowres, bad anatomy\nSteps: 28, Sampler: Euler a, Schedule type: Karras, CFG scale: 5.5, Seed: 3141592653589793238, Size: 832x1216, Model hash: aabbccdd11, Model: animagine xl 3.1, VAE: sdxl_vae.safetensors, Clip skip: 2, Version: v1.10.0, Lora hashes: \"add-detail:1234abcd, char-loom:5678efab\"",
        )]);
        let row = parse_sd_parameters(&chunks).unwrap();
        let c = &row.columns;
        assert_eq!(c.generator.as_deref(), Some("Stable Diffusion WebUI"));
        assert_eq!(
            c.positive_prompt.as_deref(),
            Some("masterpiece, best quality, 1girl\nsecond prompt line")
        );
        assert_eq!(c.negative_prompt.as_deref(), Some("lowres, bad anatomy"));
        assert_eq!(c.steps, Some(28));
        assert_eq!(c.sampler.as_deref(), Some("Euler a"));
        assert_eq!(c.scheduler.as_deref(), Some("Karras"));
        assert_eq!(c.cfg_scale, Some(5.5));
        // Seedはu64超過でも文字列で保持
        assert_eq!(c.seed.as_deref(), Some("3141592653589793238"));
        assert_eq!(c.generation_model.as_deref(), Some("animagine xl 3.1"));
        assert_eq!(c.vae.as_deref(), Some("sdxl_vae.safetensors"));
        let extra: serde_json::Value =
            serde_json::from_str(c.generation_extra.as_deref().unwrap()).unwrap();
        assert_eq!(extra["size"], serde_json::json!("832x1216"));
        assert_eq!(extra["clip_skip"], serde_json::json!(2));
        assert_eq!(extra["version"], serde_json::json!("v1.10.0"));
        assert_eq!(
            extra["loras"],
            serde_json::json!([
                {"name": "add-detail", "model_hash": "1234abcd"},
                {"name": "char-loom", "model_hash": "5678efab"},
            ])
        );
    }

    #[test]
    fn sd_params_without_negative_and_minimal() {
        let chunks = chunks_with(&[(
            "parameters",
            "simple prompt\nSteps: 20, Sampler: DPM++ 2M, CFG scale: 7, Seed: 42",
        )]);
        let c = &parse_sd_parameters(&chunks).unwrap().columns;
        assert_eq!(c.positive_prompt.as_deref(), Some("simple prompt"));
        assert!(c.negative_prompt.is_none());
        assert_eq!(c.steps, Some(20));
        // "DPM++ 2M" — モデル名と違いサンプラー名にカンマは無い
        assert_eq!(c.sampler.as_deref(), Some("DPM++ 2M"));
    }

    #[test]
    fn sd_params_model_name_with_comma() {
        let chunks = chunks_with(&[(
            "parameters",
            "p\nSteps: 20, Sampler: Euler, CFG scale: 7, Seed: 1, Model: my model, v2 final",
        )]);
        let c = &parse_sd_parameters(&chunks).unwrap().columns;
        // カンマ入りモデル名が直前の値へ連結される
        assert_eq!(c.generation_model.as_deref(), Some("my model, v2 final"));
    }

    #[test]
    fn sd_params_requires_steps_line() {
        // Steps行なし = A1111形式と断定できない → 非検出
        let chunks = chunks_with(&[("parameters", "just a prompt")]);
        assert!(parse_sd_parameters(&chunks).is_none());
        let chunks = chunks_with(&[("Title", "AI generated image")]);
        assert!(parse_sd_parameters(&chunks).is_none());
    }

    // ===== ComfyUI =====

    fn comfy_graph() -> String {
        serde_json::json!({
            "1": {"class_type": "CheckpointLoaderSimple",
                  "inputs": {"ckpt_name": "flux1-dev.safetensors"}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cinematic photo"}},
            "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry, low quality"}},
            "4": {"class_type": "KSampler",
                  "inputs": {"seed": 9876543210i64, "steps": 30, "cfg": 3.5,
                             "sampler_name": "euler", "scheduler": "simple",
                             "positive": ["2", 0], "negative": ["3", 0],
                             "denoise": 1.0}},
            "5": {"class_type": "VAELoader", "inputs": {"vae_name": "ae.safetensors"}},
            "6": {"class_type": "LoraLoader",
                  "inputs": {"lora_name": "detail-tweaker.safetensors",
                             "strength_model": 0.8}}
        })
        .to_string()
    }

    fn comfy_chunks() -> PngTextChunks {
        let prompt = comfy_graph();
        let workflow = serde_json::json!({"nodes": [], "version": 0.4, "extra": {}}).to_string();
        chunks_with(&[("prompt", prompt.as_str()), ("workflow", workflow.as_str())])
    }

    #[test]
    fn comfy_extracts_via_connection_tracing() {
        let row = parse_comfy(&comfy_chunks()).unwrap();
        let c = &row.columns;
        assert_eq!(c.generator.as_deref(), Some("ComfyUI"));
        assert_eq!(c.positive_prompt.as_deref(), Some("a cinematic photo"));
        assert_eq!(c.negative_prompt.as_deref(), Some("blurry, low quality"));
        assert_eq!(c.steps, Some(30));
        assert_eq!(c.cfg_scale, Some(3.5));
        assert_eq!(c.sampler.as_deref(), Some("euler"));
        assert_eq!(c.scheduler.as_deref(), Some("simple"));
        assert_eq!(c.seed.as_deref(), Some("9876543210"));
        assert_eq!(c.generation_model.as_deref(), Some("flux1-dev.safetensors"));
        assert_eq!(c.vae.as_deref(), Some("ae.safetensors"));
        let extra: serde_json::Value =
            serde_json::from_str(c.generation_extra.as_deref().unwrap()).unwrap();
        assert_eq!(
            extra["loras"][0]["name"],
            serde_json::json!("detail-tweaker.safetensors")
        );
        // workflowはverbatim保持
        let wf: serde_json::Value =
            serde_json::from_str(row.raw_workflow.as_deref().unwrap()).unwrap();
        assert_eq!(wf["version"], serde_json::json!(0.4));
    }

    #[test]
    fn comfy_ksampler_advanced_and_combined_conditions() {
        let graph = serde_json::json!({
            "10": {"class_type": "CLIPTextEncode", "inputs": {"text": "part one"}},
            "11": {"class_type": "CLIPTextEncode", "inputs": {"text": "part two"}},
            "12": {"class_type": "ConditioningCombine",
                   "inputs": {"conditioning_1": ["10", 0], "conditioning_2": ["11", 0]}},
            "13": {"class_type": "CLIPTextEncode", "inputs": {"text": "neg side"}},
            "14": {"class_type": "KSamplerAdvanced",
                   "inputs": {"noise_seed": 777, "steps": 25, "cfg": 7.0,
                              "sampler_name": "dpmpp_2m", "scheduler": "karras",
                              "positive": ["12", 0], "negative": ["13", 0]}}
        })
        .to_string();
        let chunks = chunks_with(&[("prompt", &graph)]);
        let row = parse_comfy(&chunks).unwrap();
        let c = &row.columns;
        assert_eq!(c.seed.as_deref(), Some("777"), "noise_seed");
        assert_eq!(
            c.positive_prompt.as_deref(),
            Some("part one\npart two"),
            "ConditioningCombine経由の連結"
        );
        assert_eq!(c.negative_prompt.as_deref(), Some("neg side"));
        assert!(row.raw_workflow.is_none(), "workflowチャンクなし");
    }

    #[test]
    fn comfy_invalid_json_not_detected() {
        let chunks = chunks_with(&[("prompt", "{not json")]);
        assert!(parse_comfy(&chunks).is_none());
        // promptチャンクなし
        let chunks = chunks_with(&[("Title", "AI generated image")]);
        assert!(parse_comfy(&chunks).is_none());
    }

    #[test]
    fn comfy_unresolvable_positive_is_none() {
        // positiveが未知ノードを参照 → 抽出不能 (unknown扱い) でも行自体は
        // generator/raw_workflow保持で成立
        let graph = serde_json::json!({
            "1": {"class_type": "UnknownNode", "inputs": {}},
            "2": {"class_type": "KSampler",
                  "inputs": {"seed": 1, "steps": 20, "cfg": 1.0,
                             "sampler_name": "euler", "scheduler": "normal",
                             "positive": ["1", 0], "negative": ["1", 0]}}
        })
        .to_string();
        let chunks = chunks_with(&[("prompt", &graph)]);
        let c = &parse_comfy(&chunks).unwrap().columns;
        assert!(c.positive_prompt.is_none());
        assert_eq!(c.steps, Some(20));
    }

    // ===== NovelAI =====

    #[test]
    fn nai_comment_json_form() {
        let comment = serde_json::json!({
            "prompt": "masterpiece, 1girl, azure hair",
            "steps": 28,
            "height": 1216, "width": 832,
            "scale": 5.0,
            "uc": "lowres, bad hands",
            "seed": 1234567890123456789i64,
            "sampler": "k_euler",
            "n_samples": 1,
            "ucPreset": 0
        })
        .to_string();
        let chunks = chunks_with(&[
            ("Title", "AI generated image"),
            ("Description", "masterpiece, 1girl, azure hair"),
            ("Software", "NovelAI"),
            ("Source", "Stable Diffusion XL C1E1DE52"),
            ("Comment", &comment),
        ]);
        let row = parse_novelai(&chunks).unwrap();
        let c = &row.columns;
        assert_eq!(c.generator.as_deref(), Some("NovelAI"));
        assert_eq!(
            c.positive_prompt.as_deref(),
            Some("masterpiece, 1girl, azure hair")
        );
        assert_eq!(c.negative_prompt.as_deref(), Some("lowres, bad hands"));
        assert_eq!(c.steps, Some(28));
        assert_eq!(c.cfg_scale, Some(5.0));
        assert_eq!(c.seed.as_deref(), Some("1234567890123456789"));
        assert_eq!(c.sampler.as_deref(), Some("k_euler"));
        assert_eq!(
            c.generation_model.as_deref(),
            Some("Stable Diffusion XL C1E1DE52")
        );
        let extra: serde_json::Value =
            serde_json::from_str(c.generation_extra.as_deref().unwrap()).unwrap();
        assert_eq!(extra["width"], serde_json::json!(832));
        assert!(extra.get("prompt").is_none(), "主要キーはextraに重複しない");
    }

    #[test]
    fn nai_requires_signature() {
        // Software無し + Comment非JSON → 非検出
        let chunks = chunks_with(&[("Title", "AI generated image")]);
        assert!(parse_novelai(&chunks).is_none());
        // promptキーを持つJSON CommentならSoftware無しでも検出
        let chunks = chunks_with(&[("Comment", r#"{"prompt": "p", "steps": 10}"#)]);
        assert!(parse_novelai(&chunks).is_some());
    }
}
