use anyhow::{bail, Context, Result};
use calamine::{open_workbook_auto, Data, Reader};
use chrono::{Local, NaiveDate};
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub const EU: &[&str] = &[
    "DE", "FR", "IT", "ES", "IE", "NL", "BE", "AT", "PL", "CZ", "HU", "RO", "BG", "HR", "SI", "SK",
    "LT", "LV", "EE", "LU", "MT", "CY", "SE", "TR",
];
pub const PERIODS: &[&str] = &["7", "14", "30", "60", "90"];

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum Value {
    Text(String),
    Number(f64),
    Blank,
}
impl Value {
    pub fn blank(&self) -> bool {
        matches!(self, Self::Blank) || matches!(self, Self::Text(s) if s.is_empty())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Source {
    pub path: PathBuf,
    pub sheet: String,
    pub country: String,
    pub rows: usize,
    pub columns: usize,
    pub converted: usize,
    pub encoding: String,
    pub modified: String,
    pub hash: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub target: PathBuf,
    pub station: String,
    pub code: String,
    pub date: String,
    pub root: PathBuf,
    pub sources: Vec<Source>,
    pub warnings: Vec<String>,
    pub target_hash: String,
    #[serde(skip)]
    pub sheets: BTreeMap<String, Vec<Vec<Value>>>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub target: PathBuf,
    pub backup: Option<PathBuf>,
    pub sheets: usize,
    pub converted: usize,
    pub message: String,
}

pub fn hash(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
fn station(code: &str) -> Option<&str> {
    if EU.contains(&code) || code == "EU" {
        Some("EU")
    } else if ["US", "CA", "UK", "JP"].contains(&code) {
        Some(code)
    } else {
        None
    }
}
fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}
fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for e in fs::read_dir(dir).with_context(|| format!("无法读取目录：{}", dir.display()))? {
        let p = e?.path();
        let n = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if p.is_file()
            && !n.starts_with("~$")
            && !n.starts_with('.')
            && !n.contains("backup")
            && !n.contains("备份")
            && ["xlsx", "xls", "csv", "txt", "tsv"].contains(&extension(&p).as_str())
        {
            result.push(p);
        }
    }
    Ok(result)
}
fn latest(
    mut candidates: Vec<PathBuf>,
    warnings: &mut Vec<String>,
    label: &str,
) -> Result<Option<PathBuf>> {
    candidates.sort_by_key(|p| {
        (
            fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH),
            p.clone(),
        )
    });
    if candidates.len() > 1 {
        warnings.push(format!(
            "{label}有 {} 个候选文件，按修改时间选取最新文件，请核对。",
            candidates.len()
        ));
    }
    Ok(candidates.pop())
}
fn report_file(
    dir: &Path,
    code: &str,
    period: &str,
    date: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<PathBuf>> {
    let stem = format!("AVT-{code}-{period}T-{date}");
    latest(
        files(dir)?
            .into_iter()
            .filter(|p| {
                p.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&stem)
            })
            .collect(),
        warnings,
        &format!("{code} {period}天销"),
    )
}

// Fixed positional rules: zero-based columns, no header inference or reordering.
pub fn count_columns(station: &str) -> &'static [usize] {
    if ["EU", "UK"].contains(&station) {
        &[3, 5, 8, 9, 13, 14]
    } else {
        &[3, 4, 7, 8, 13, 14, 19, 20]
    }
}
pub fn plain_number(s: &str) -> Option<f64> {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let r = PATTERN.get_or_init(|| {
        Regex::new(r"^[+-]?(?:\d+(?:\.\d+)?|\d{1,3}(?:,\d{3})+(?:\.\d+)?)$").unwrap()
    });
    let trimmed = s.trim();
    if !r.is_match(trimmed) {
        return None;
    }
    let significant = trimmed.chars().filter(|c| c.is_ascii_digit()).count();
    if significant > 15 {
        return None;
    }
    let n: f64 = trimmed.replace(',', "").parse().ok()?;
    n.is_finite().then_some(n)
}

// Sales conversion is restricted to integer count columns. Accept grouped
// thousands without treating a European thousands dot as a decimal point.
pub fn count_number(s: &str) -> Option<f64> {
    static GROUPED: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let grouped = GROUPED
        .get_or_init(|| Regex::new(r"^[+-]?\d{1,3}(?:[., \x{00A0}\x{202F}]\d{3})+$").unwrap());
    let s = s.trim();
    if grouped.is_match(s) {
        let separators: std::collections::BTreeSet<char> = s
            .chars()
            .filter(|c| !c.is_ascii_digit() && *c != '+' && *c != '-')
            .collect();
        if separators.len() != 1 {
            return None;
        }
        let digits: String = s
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+' || *c == '-')
            .collect();
        return plain_number(&digits);
    }
    plain_number(s).filter(|n| n.fract() == 0.0)
}
fn decode(bytes: &[u8], code: &str) -> Result<(String, String)> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let enc = if bytes[0] == 0xff {
            encoding_rs::UTF_16LE
        } else {
            encoding_rs::UTF_16BE
        };
        let (s, _, bad) = enc.decode(bytes);
        if bad {
            bail!("UTF-16 编码损坏");
        }
        return Ok((s.into_owned(), enc.name().into()));
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok((s.trim_start_matches('\u{feff}').into(), "UTF-8".into()));
    }
    let encs = if code == "JP" {
        vec![encoding_rs::SHIFT_JIS, encoding_rs::GBK]
    } else {
        vec![encoding_rs::GBK, encoding_rs::WINDOWS_1252]
    };
    for enc in encs {
        let (s, _, bad) = enc.decode(bytes);
        if !bad {
            return Ok((s.into_owned(), enc.name().into()));
        }
    }
    bail!("无法可靠识别文件编码，请将源报告另存为 UTF-8 CSV 或 XLSX")
}
pub fn read_table(path: &Path, code: &str) -> Result<(Vec<Vec<Value>>, String)> {
    if ["xlsx", "xls"].contains(&extension(path).as_str()) {
        let mut wb = open_workbook_auto(path).context("无法读取源 Excel")?;
        let range = wb.worksheet_range_at(0).context("源文件没有工作表")??;
        let mut table = Vec::new();
        for row in range.rows() {
            let mut output = vec![];
            for v in row {
                output.push(match v {
                    Data::Empty => Value::Blank,
                    Data::Int(n) => Value::Number(*n as f64),
                    Data::Float(n) => Value::Number(*n),
                    Data::Error(e) => bail!("源文件包含错误单元格：{e:?}"),
                    Data::String(s) => Value::Text(s.clone()),
                    _ => Value::Text(v.to_string()),
                });
            }
            table.push(output);
        }
        return Ok((table, "Excel".into()));
    }
    let bytes = fs::read(path)?;
    let (s, encoding) = decode(&bytes, code)?;
    let mut best: Option<Vec<Vec<Value>>> = None;
    let mut width = 0;
    for delimiter in *b"\t,;|" {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .delimiter(delimiter)
            .flexible(false)
            .from_reader(s.as_bytes());
        let parsed: std::result::Result<Vec<_>, _> = reader.records().collect();
        if let Ok(rows) = parsed {
            let w = rows.first().map(|r| r.len()).unwrap_or(0);
            if w > width && rows.len() > 1 {
                width = w;
                best = Some(
                    rows.iter()
                        .map(|r| {
                            r.iter()
                                .map(|s| {
                                    if s.is_empty() {
                                        Value::Blank
                                    } else {
                                        Value::Text(s.into())
                                    }
                                })
                                .collect()
                        })
                        .collect(),
                );
            }
        }
    }
    if width < 2 {
        bail!(
            "无法解析多列表格，可能存在编码、引号或分隔符问题：{}",
            path.display()
        );
    }
    Ok((best.unwrap(), encoding))
}
fn load(
    plan: &mut Plan,
    path: PathBuf,
    sheet: &str,
    code: &str,
    sales: bool,
) -> Result<Vec<Vec<Value>>> {
    let before = hash(&path)?;
    let (mut rows, encoding) =
        read_table(&path, code).with_context(|| format!("读取 {} 失败", path.display()))?;
    while rows.last().is_some_and(|r| r.iter().all(Value::blank)) {
        rows.pop();
    }
    if rows.len() < 2 {
        bail!("源文件没有数据行：{}", path.display());
    }
    let columns = rows[0].len();
    if sales {
        let expected = if ["EU", "UK"].contains(&plan.station.as_str()) {
            15
        } else {
            21
        };
        if columns != expected {
            bail!("{} 应为 {expected} 列，实际 {columns} 列。固定列转换已停止，请使用与模板列数一致的报告。", path.display());
        }
    }
    let mut converted = 0;
    for row in rows.iter_mut().skip(1) {
        for (col, v) in row.iter_mut().enumerate() {
            // Non-sales CSV inventory needs pandas-like numeric storage, with identifiers protected.
            let numeric = if sales {
                count_columns(&plan.station).contains(&col)
            } else if sheet == "FBA库存" {
                col >= if ["EU", "CA"].contains(&plan.station.as_str()) {
                    5
                } else {
                    6
                }
            } else {
                col >= 9
            };
            if numeric {
                if let Value::Text(s) = v {
                    if let Some(n) = if sales {
                        count_number(s)
                    } else {
                        plain_number(s)
                    } {
                        *v = Value::Number(n);
                        if sales {
                            converted += 1;
                        }
                    }
                }
            }
        }
    }
    if hash(&path)? != before {
        bail!("读取期间源文件发生变化，请重新识别");
    }
    let modified: chrono::DateTime<Local> = fs::metadata(&path)?.modified()?.into();
    plan.sources.push(Source {
        path,
        sheet: sheet.into(),
        country: code.into(),
        rows: rows.len() - 1,
        columns,
        converted,
        encoding,
        modified: modified.format("%Y-%m-%d %H:%M:%S").to_string(),
        hash: before,
    });
    Ok(rows)
}
pub fn inspect(target: &Path) -> Result<Plan> {
    let target = fs::canonicalize(target).context("找不到补货表")?;
    if extension(&target) != "xlsx" {
        bail!("补货计划必须为 .xlsx；源数据支持 CSV、TXT、TSV、XLS、XLSX");
    }
    let name = target.file_name().unwrap().to_string_lossy();
    let r = Regex::new(r"(?i)^AVT-([A-Z]{2})-(\d{8}).*全站点通用补货计划\.xlsx$")?;
    let cap = r
        .captures(&name)
        .context("文件名应为 AVT-站点-YYYYMMDD…全站点通用补货计划.xlsx")?;
    let code = cap[1].to_ascii_uppercase();
    let date = cap[2].to_string();
    NaiveDate::parse_from_str(&date, "%Y%m%d").context("补货表日期无效")?;
    let site = station(&code).context("不支持此站点")?.to_string();
    let root = target.parent().context("无法确定站点目录")?.to_path_buf();
    let folder = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_uppercase();
    let folder_code = folder.strip_prefix("AVT-").unwrap_or(&folder);
    if let Some(folder_site) = station(folder_code) {
        if folder_site != site {
            bail!("文件名站点 {site} 与目录 {folder} 不一致");
        }
    }
    let target_hash = hash(&target)?;
    let mut plan = Plan {
        target,
        station: site,
        code,
        date,
        root,
        target_hash,
        sources: vec![],
        warnings: vec![],
        sheets: BTreeMap::new(),
    };
    let names = crate::xlsx::sheet_names(&plan.target)?;
    for sheet in ["FBA库存", "限制发货数量"] {
        if let Some(p) = latest(files(&plan.root.join(sheet))?, &mut plan.warnings, sheet)? {
            let code = plan.code.clone();
            let rows = load(&mut plan, p, sheet, &code, false)?;
            plan.sheets.insert(sheet.into(), rows);
        } else {
            plan.warnings
                .push(format!("缺少 {sheet} 源文件，该工作表将保留原数据。"));
        }
    }
    let sales_dir = plan.root.join("业务报告").join(&plan.date);
    for period in PERIODS {
        let sheet = format!("{period}天销");
        if plan.station == "EU" {
            let mut combined = vec![];
            for country in EU {
                if let Some(p) = report_file(
                    &sales_dir.join(country),
                    country,
                    period,
                    &plan.date,
                    &mut plan.warnings,
                )? {
                    let mut rows = load(&mut plan, p, &sheet, country, true)?;
                    rows[0].resize(17, Value::Blank);
                    rows[0][16] = Value::Text(country.to_string());
                    if !combined.is_empty() {
                        combined.push(vec![]);
                        combined.push(vec![]);
                    }
                    combined.extend(rows);
                } else if sales_dir.join(country).is_dir() {
                    plan.warnings
                        .push(format!("{country} 缺少 {period}天报告，此周期未包含该国。"));
                }
            }
            if !combined.is_empty() {
                plan.sheets.insert(sheet.clone(), combined);
            }
        } else {
            let code = plan.code.clone();
            let primary = report_file(&sales_dir, &code, period, &plan.date, &mut plan.warnings)?;
            if let Some(p) = primary {
                let rows = load(&mut plan, p, &sheet, &code, true)?;
                plan.sheets.insert(sheet.clone(), rows);
            }
            if plan.station == "US" {
                if let Some(p) = report_file(
                    &sales_dir.join("MX"),
                    "MX",
                    period,
                    &plan.date,
                    &mut plan.warnings,
                )? {
                    let rows = load(&mut plan, p, &sheet, "MX", true)?;
                    if let Some(existing) = plan.sheets.get_mut(&sheet) {
                        existing.push(vec![]);
                        existing.push(vec![]);
                        existing.extend(rows.into_iter().skip(1));
                    } else {
                        plan.warnings.push(format!(
                            "{sheet} 缺少 US，仅使用 MX 报告（沿用原脚本规则）。"
                        ));
                        plan.sheets.insert(sheet.clone(), rows);
                    }
                } else if sales_dir.join("MX").is_dir() {
                    plan.warnings
                        .push(format!("MX 缺少 {period}天报告，此周期不追加 MX。"));
                }
            }
        }
        if !plan.sheets.contains_key(&sheet) {
            plan.warnings
                .push(format!("缺少 {sheet} 源文件，该工作表将保留原数据。"));
        }
    }
    for sheet in plan.sheets.keys() {
        if !names.contains(sheet) {
            bail!("目标文件缺少工作表：{sheet}，未修改原表");
        }
    }
    if plan.sheets.is_empty() {
        bail!("未找到可填充数据。请把补货表放在含 FBA库存、限制发货数量、业务报告 的站点文件夹中。识别目录：{}", plan.root.display());
    }
    if hash(&plan.target)? != plan.target_hash {
        bail!("识别期间补货表发生变化，请重试");
    }
    Ok(plan)
}

pub fn execute(plan: &Plan, backup: bool) -> Result<Outcome> {
    if hash(&plan.target)? != plan.target_hash {
        bail!("补货表在识别后已变化，请重新识别再填充");
    }
    for s in &plan.sources {
        if hash(&s.path)? != s.hash {
            bail!("源文件在识别后已变化，请重新识别：{}", s.path.display());
        }
    }
    let backup_path = crate::xlsx::write_plan(plan, backup)?;
    let converted = plan.sources.iter().map(|s| s.converted).sum();
    let outcome = Outcome {
        target: plan.target.clone(),
        backup: backup_path,
        sheets: plan.sheets.len(),
        converted,
        message:
            "填充完成。打开 Excel / WPS 后由表格软件重算公式；原模板已有公式错误不在本次修复范围。"
                .into(),
    };
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn number_rules() {
        assert_eq!(plain_number("1,185"), Some(1185.0));
        assert_eq!(plain_number(" 123 "), Some(123.0));
        for s in [
            "23.71%",
            "£299.37",
            "￥1,000",
            "1.234,56",
            "1,23",
            "365+",
            "2026-08-24",
            "1234567890123456",
        ] {
            assert_eq!(plain_number(s), None, "{s}");
        }
    }
    #[test]
    fn fixed_columns_protect_currency_and_rates() {
        for site in ["US", "CA", "JP"] {
            for c in [0, 1, 2, 5, 6, 9, 10, 11, 12, 15, 16, 17, 18] {
                assert!(!count_columns(site).contains(&c));
            }
        }
        for site in ["UK", "EU"] {
            for c in [0, 1, 2, 4, 6, 7, 10, 11, 12] {
                assert!(!count_columns(site).contains(&c));
            }
        }
    }

    #[test]
    fn count_thousands_are_unambiguous() {
        for s in ["1,185", "1.185", "1 185", "1\u{00a0}185", "1\u{202f}185"] {
            assert_eq!(count_number(s), Some(1185.0), "{s}");
        }
        for s in ["1.185,000", "23.71%", "£1,185", "12.5"] {
            assert_eq!(count_number(s), None, "{s}");
        }
    }
}
