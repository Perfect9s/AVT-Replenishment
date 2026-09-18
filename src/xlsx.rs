use crate::core::{hash, Plan, Value};
use anyhow::{bail, Context, Result};
use chrono::Local;
use roxmltree::{Document, Node};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::{write::FileOptions, CompressionMethod, ZipArchive, ZipWriter};

fn read_part(zip: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let mut s = String::new();
    zip.by_name(name)?.read_to_string(&mut s)?;
    Ok(s)
}
fn mappings(zip: &mut ZipArchive<File>) -> Result<BTreeMap<String, String>> {
    let wb = read_part(zip, "xl/workbook.xml")?;
    let rel = read_part(zip, "xl/_rels/workbook.xml.rels")?;
    let w = Document::parse(&wb)?;
    let r = Document::parse(&rel)?;
    let mut map = BTreeMap::new();
    for sheet in w.descendants().filter(|n| n.has_tag_name("sheet")) {
        let id = sheet
            .attribute((
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
                "id",
            ))
            .context("工作表缺少关系 ID")?;
        let target = r
            .descendants()
            .find(|n| n.attribute("Id") == Some(id))
            .and_then(|n| n.attribute("Target"))
            .context("工作表关系无效")?;
        let path = if target.starts_with('/') {
            target.trim_start_matches('/').into()
        } else {
            format!("xl/{target}")
        };
        map.insert(
            sheet.attribute("name").context("工作表无名称")?.into(),
            path,
        );
    }
    Ok(map)
}
pub fn sheet_names(path: &Path) -> Result<Vec<String>> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    Ok(mappings(&mut zip)?.into_keys().collect())
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
}
fn col_name(mut col: usize) -> String {
    let mut s = String::new();
    while col > 0 {
        col -= 1;
        s.insert(0, (b'A' + (col % 26) as u8) as char);
        col /= 26;
    }
    s
}
fn col_number(s: &str) -> usize {
    s.bytes()
        .take_while(u8::is_ascii_alphabetic)
        .fold(0, |a, c| {
            a * 26 + (c.to_ascii_uppercase() - b'A' + 1) as usize
        })
}
fn edits(xml: &str, mut parts: Vec<(std::ops::Range<usize>, String)>) -> String {
    parts.sort_by_key(|p| std::cmp::Reverse(p.0.start));
    let mut out = xml.to_string();
    for (range, s) in parts {
        out.replace_range(range, &s);
    }
    out
}
fn attrs(n: Node<'_, '_>, skip: &[&str]) -> String {
    n.attributes()
        .filter(|a| !skip.contains(&a.name()) && a.namespace().is_none())
        .map(|a| format!(" {}=\"{}\"", a.name(), esc(a.value())))
        .collect()
}
pub fn patch_sheet(xml: &str, rows: &[Vec<Value>]) -> Result<String> {
    if rows.len() > 1_048_576 || rows.iter().any(|r| r.len() > 16_384) {
        bail!("源数据超出 Excel 工作表行列限制");
    }
    let doc = Document::parse(xml)?;
    let data = doc
        .descendants()
        .find(|n| n.has_tag_name("sheetData"))
        .context("工作表没有 sheetData")?;
    let mut old_rows = BTreeMap::new();
    let mut old_cells: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for row in data.children().filter(|n| n.has_tag_name("row")) {
        let ri: usize = row.attribute("r").context("row missing r")?.parse()?;
        old_rows.insert(ri, attrs(row, &["r", "spans"]));
        for c in row.children().filter(|n| n.has_tag_name("c")) {
            if let (Some(s), Some(r)) = (c.attribute("s"), c.attribute("r")) {
                old_cells.insert((ri, col_number(r)), s.to_string());
            }
        }
    }
    let max_row = rows.len().max(*old_rows.keys().next_back().unwrap_or(&0));
    let mut out = String::from("<sheetData>");
    for ri in 1..=max_row {
        let current = rows.get(ri - 1);
        let old_max = old_cells
            .range((ri, 0)..=(ri, usize::MAX))
            .map(|((_, c), _)| *c)
            .max()
            .unwrap_or(0);
        let max_col = old_max.max(current.map(Vec::len).unwrap_or(0));
        if max_col == 0 && !old_rows.contains_key(&ri) {
            continue;
        }
        out.push_str(&format!(
            "<row r=\"{ri}\"{}>",
            old_rows.get(&ri).map(String::as_str).unwrap_or("")
        ));
        for ci in 1..=max_col {
            let value = current.and_then(|r| r.get(ci - 1)).unwrap_or(&Value::Blank);
            let style = old_cells.get(&(ri, ci)).or_else(|| old_cells.get(&(2, ci)));
            if value.blank() && !old_cells.contains_key(&(ri, ci)) {
                continue;
            }
            let st = style.map(|s| format!(" s=\"{s}\"")).unwrap_or_default();
            let coord = format!("{}{ri}", col_name(ci));
            match value {
                Value::Blank => out.push_str(&format!("<c r=\"{coord}\"{st}/>")),
                Value::Number(n) => {
                    if !n.is_finite() {
                        bail!("不能写入非有限数值");
                    }
                    out.push_str(&format!("<c r=\"{coord}\"{st} t=\"n\"><v>{n}</v></c>"));
                }
                Value::Text(s) => {
                    if s.chars().any(|c| {
                        (c < ' ' && !['\t', '\n', '\r'].contains(&c))
                            || ['\u{fffe}', '\u{ffff}'].contains(&c)
                    }) {
                        bail!("源报告包含 XML 不允许的控制字符");
                    }
                    out.push_str(&format!("<c r=\"{coord}\"{st} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",esc(s)));
                }
            }
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData>");
    let mut replacements = vec![(data.range(), out)];
    if let Some(d) = doc.descendants().find(|n| n.has_tag_name("dimension")) {
        let new_col = rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .max(old_cells.keys().map(|(_, c)| *c).max().unwrap_or(1));
        replacements.push((
            d.range(),
            format!(
                "<dimension ref=\"A1:{}{}\"/>",
                col_name(new_col),
                max_row.max(1)
            ),
        ));
    }
    let result = edits(xml, replacements);
    Document::parse(&result).context("生成的工作表 XML 无效")?;
    Ok(result)
}
fn recalc(xml: &str) -> Result<String> {
    let doc = Document::parse(xml)?;
    let calc = "<calcPr calcId=\"0\" calcMode=\"auto\" fullCalcOnLoad=\"1\" forceFullCalc=\"1\"/>";
    if let Some(n) = doc.descendants().find(|n| n.has_tag_name("calcPr")) {
        // Preserve iteration and precision settings on the original workbook.
        let s = format!(
            "<calcPr{} calcId=\"0\" calcMode=\"auto\" fullCalcOnLoad=\"1\" forceFullCalc=\"1\"/>",
            attrs(
                n,
                &["calcId", "calcMode", "fullCalcOnLoad", "forceFullCalc"]
            )
        );
        Ok(edits(xml, vec![(n.range(), s)]))
    } else {
        let pos = xml
            .rfind("</workbook>")
            .context("workbook closing tag missing")?;
        Ok(edits(xml, vec![(pos..pos, calc.into())]))
    }
}

pub fn write_plan(plan: &Plan, backup: bool) -> Result<Option<PathBuf>> {
    let lock = plan.root.join(format!(
        "~${}",
        plan.target.file_name().unwrap().to_string_lossy()
    ));
    if lock.exists() {
        bail!("补货表可能正在 Excel / WPS 中打开，请关闭后重试");
    }
    let file = {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        options
            .open(&plan.target)
            .context("无法读取补货表，请关闭 Excel / WPS 后重试")?
    };
    let mut zip = ZipArchive::new(file)?;
    let map = mappings(&mut zip)?;
    let mut changed = BTreeMap::new();
    for (sheet, rows) in &plan.sheets {
        let path = map.get(sheet).context("目标工作表消失")?;
        changed.insert(
            path.clone(),
            patch_sheet(&read_part(&mut zip, path)?, rows)?,
        );
    }
    changed.insert(
        "xl/workbook.xml".into(),
        recalc(&read_part(&mut zip, "xl/workbook.xml")?)?,
    );
    let mut tmp = tempfile::Builder::new()
        .prefix(".avt-writing-")
        .suffix(".xlsx")
        .tempfile_in(&plan.root)?;
    {
        let mut writer = ZipWriter::new(tmp.as_file_mut());
        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            if let Some(s) = changed.get(entry.name()) {
                writer.start_file(
                    entry.name(),
                    FileOptions::default().compression_method(CompressionMethod::Deflated),
                )?;
                writer.write_all(s.as_bytes())?;
            } else {
                writer.raw_copy_file(entry)?;
            }
        }
        writer.finish()?;
    }
    tmp.as_file().sync_all()?;
    // Read back every ZIP member, validate edited XML, and byte-compare all preserved parts.
    let mut verify = ZipArchive::new(File::open(tmp.path())?)?;
    if verify.len() != zip.len() {
        bail!("保存校验失败：文件部件数量变化");
    }
    for i in 0..zip.len() {
        let mut old = zip.by_index(i)?;
        let name = old.name().to_string();
        let mut bytes = Vec::new();
        verify.by_name(&name)?.read_to_end(&mut bytes)?;
        if let Some(expected) = changed.get(&name) {
            if bytes != expected.as_bytes() {
                bail!("写入校验失败：{name}");
            }
            Document::parse(std::str::from_utf8(&bytes)?)?;
        } else {
            let mut original = Vec::new();
            old.read_to_end(&mut original)?;
            if original != bytes {
                bail!("模板保留校验失败：{name}");
            }
        }
    }
    drop(verify);
    if hash(&plan.target)? != plan.target_hash {
        bail!("保存前发现原表发生变化，未覆盖");
    }
    let backup_path = if backup {
        let p = plan.root.join(format!(
            "{}_backup_{}.xlsx",
            plan.target.file_stem().unwrap().to_string_lossy(),
            Local::now().format("%Y%m%d_%H%M%S_%f")
        ));
        let mut backup_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .context("无法创建备份，已停止填充")?;
        let mut source = File::open(&plan.target)?;
        std::io::copy(&mut source, &mut backup_file)?;
        backup_file.sync_all()?;
        if hash(&p)? != plan.target_hash {
            bail!("备份校验失败，未覆盖原表");
        }
        Some(p)
    } else {
        None
    };
    drop(zip);
    // No delete-then-rename: replacement must either succeed or leave the original intact.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn ReplaceFileW(
                replaced: *const u16,
                replacement: *const u16,
                backup: *const u16,
                flags: u32,
                exclude: *mut std::ffi::c_void,
                reserved: *mut std::ffi::c_void,
            ) -> i32;
        }
        let from: Vec<u16> = tmp
            .path()
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let to: Vec<u16> = plan
            .target
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let temp_path = tmp.into_temp_path();
        let ok = unsafe {
            ReplaceFileW(
                to.as_ptr(),
                from.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("无法替换原表，请关闭 Excel / WPS，并确认目录可写");
        }
        drop(temp_path);
    }
    #[cfg(not(windows))]
    {
        tmp.persist(&plan.target)
            .map_err(|e| e.error)
            .context("无法替换原表")?;
    }
    Ok(backup_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patch_preserves_layout_and_removes_tail() {
        let xml = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:B3"/><cols><col min="1" max="1" width="20"/></cols><sheetData><row r="1"><c r="A1" t="str"><v>old</v></c></row><row r="2" ht="25"><c r="A2" s="2"><v>9</v></c></row><row r="3"><c r="A3" s="2"><v>999</v></c></row></sheetData><autoFilter ref="A1:B3"/></worksheet>"#;
        let patched = patch_sheet(
            xml,
            &[
                vec![Value::Text("SKU".into())],
                vec![Value::Number(1185.0), Value::Text("=1+1 & <x>".into())],
            ],
        )
        .unwrap();
        assert!(patched.contains("<cols><col min=\"1\" max=\"1\" width=\"20\"/></cols>"));
        assert!(patched.contains("ht=\"25\""));
        assert!(patched.contains("<v>1185</v>"));
        assert!(!patched.contains("999"));
        assert!(patched.contains("=1+1 &amp; &lt;x&gt;"));
    }
}
