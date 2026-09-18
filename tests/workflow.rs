use avt_replenishment::core::{execute, hash, inspect, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use zip::{write::FileOptions, ZipArchive, ZipWriter};

fn template(dir: &Path, code: &str) -> PathBuf {
    let target = dir.join(format!("AVT-{code}-20260824-- 全站点通用补货计划.xlsx"));
    let mut z = ZipWriter::new(fs::File::create(&target).unwrap());
    let sheets = [
        "FBA库存",
        "限制发货数量",
        "7天销",
        "14天销",
        "30天销",
        "60天销",
        "90天销",
        "补货汇总",
    ];
    let mut wb = String::from(
        r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>"#,
    );
    let mut rel = String::from(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (i, name) in sheets.iter().enumerate() {
        wb.push_str(&format!(
            "<sheet name=\"{name}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
            i + 1,
            i + 1
        ));
        rel.push_str(&format!(
            "<Relationship Id=\"rId{}\" Target=\"worksheets/sheet{}.xml\"/>",
            i + 1,
            i + 1
        ));
        z.start_file(
            format!("xl/worksheets/sheet{}.xml", i + 1),
            FileOptions::default(),
        )
        .unwrap();
        z.write_all(br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:A4"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>old</t></is></c></row><row r="4"><c r="A4"><f>SUM(A2:A3)</f><v>99</v></c></row></sheetData></worksheet>"#).unwrap();
    }
    wb.push_str("</sheets><calcPr iterate=\"1\" iterateCount=\"88\"/></workbook>");
    rel.push_str("</Relationships>");
    for (name, data) in [
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rel.as_bytes()),
        ("xl/externalLinks/externalLink1.xml", b"external-untouched"),
    ] {
        z.start_file(name, FileOptions::default()).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
    target
}
fn report(path: &Path, width: usize, marker: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut w = csv::Writer::from_path(path).unwrap();
    w.write_record((0..width).map(|i| format!("Column{i}")))
        .unwrap();
    let mut row = vec!["23.71%".to_string(); width];
    row[0] = "001234".into();
    row[1] = marker.into();
    row[2] = "=1+1".into();
    row[3] = "1,185".into();
    row[if width == 15 { 11 } else { 17 }] = "£2,052.55".into();
    w.write_record(row).unwrap();
    w.flush().unwrap();
}
fn setup(code: &str) -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join(format!("AVT-{code}"));
    fs::create_dir(&root).unwrap();
    let target = template(&root, code);
    (t, target)
}
#[test]
fn replacement_and_backup_preserve_unrelated_parts() {
    let (_t, target) = setup("UK");
    let root = target.parent().unwrap();
    report(
        &root.join("业务报告/20260824/AVT-UK-7T-20260824.csv"),
        15,
        "UK",
    );
    let before = hash(&target).unwrap();
    let plan = inspect(&target).unwrap();
    assert_eq!(plan.sheets["7天销"][1][3], Value::Number(1185.0));
    assert_eq!(plan.sheets["7天销"][1][0], Value::Text("001234".into()));
    assert_eq!(plan.sheets["7天销"][1][4], Value::Text("23.71%".into()));
    assert_eq!(plan.sheets["7天销"][1][11], Value::Text("£2,052.55".into()));
    let result = execute(&plan, true).unwrap();
    assert_eq!(hash(result.backup.as_ref().unwrap()).unwrap(), before);
    let mut zip = ZipArchive::new(fs::File::open(&target).unwrap()).unwrap();
    use std::io::Read;
    let mut summary = String::new();
    zip.by_name("xl/worksheets/sheet8.xml")
        .unwrap()
        .read_to_string(&mut summary)
        .unwrap();
    assert!(summary.contains("<f>SUM(A2:A3)</f>"));
    let mut sheet = String::new();
    zip.by_name("xl/worksheets/sheet3.xml")
        .unwrap()
        .read_to_string(&mut sheet)
        .unwrap();
    assert!(!sheet.contains("SUM("));
    assert!(sheet.contains("£2,052.55"));
    let mut wb = String::new();
    zip.by_name("xl/workbook.xml")
        .unwrap()
        .read_to_string(&mut wb)
        .unwrap();
    assert!(wb.contains("iterateCount=\"88\""));
    assert!(execute(&plan, false).is_err());
}
#[test]
fn us_mx_and_eu_order_and_gaps() {
    let (_t, target) = setup("US");
    let root = target.parent().unwrap();
    report(
        &root.join("业务报告/20260824/AVT-US-7T-20260824.csv"),
        21,
        "US",
    );
    report(
        &root.join("业务报告/20260824/MX/AVT-MX-7T-20260824.csv"),
        21,
        "MX",
    );
    let p = inspect(&target).unwrap();
    let rows = &p.sheets["7天销"];
    assert_eq!(rows.len(), 5);
    assert!(rows[2].is_empty() && rows[3].is_empty());
    assert_eq!(rows[4][1], Value::Text("MX".into()));
    let (_e, target) = setup("FR");
    let root = target.parent().unwrap();
    report(
        &root.join("业务报告/20260824/FR/AVT-FR-7T-20260824.csv"),
        15,
        "FR",
    );
    report(
        &root.join("业务报告/20260824/DE/AVT-DE-7T-20260824.csv"),
        15,
        "DE",
    );
    let p = inspect(&target).unwrap();
    assert_eq!(p.station, "EU");
    let rows = &p.sheets["7天销"];
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0][16], Value::Text("DE".into()));
    assert_eq!(rows[4][16], Value::Text("FR".into()));
}
#[test]
fn source_mutation_blocks_overwrite() {
    let (_t, target) = setup("JP");
    let src = target
        .parent()
        .unwrap()
        .join("业务报告/20260824/AVT-JP-7T-20260824.csv");
    report(&src, 21, "JP");
    let p = inspect(&target).unwrap();
    let before = hash(&target).unwrap();
    fs::write(src, "changed").unwrap();
    assert!(execute(&p, false).is_err());
    assert_eq!(hash(&target).unwrap(), before);
}
#[test]
fn rejects_cross_station_and_wrong_width() {
    let (_t, target) = setup("CA");
    let root = target.parent().unwrap();
    report(
        &root.join("业务报告/20260824/AVT-US-7T-20260824.csv"),
        21,
        "US",
    );
    assert!(inspect(&target).is_err());
    report(
        &root.join("业务报告/20260824/AVT-CA-7T-20260824.csv"),
        15,
        "CA",
    );
    assert!(inspect(&target).unwrap_err().to_string().contains("21"));
}

#[test]
fn backup_off_and_lock_detection() {
    let (_t, target) = setup("CA");
    let root = target.parent().unwrap();
    report(
        &root.join("业务报告/20260824/AVT-CA-7T-20260824.csv"),
        21,
        "CA",
    );
    let plan = inspect(&target).unwrap();
    let before = hash(&target).unwrap();
    let lock = root.join(format!(
        "~${}",
        target.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&lock, "").unwrap();
    assert!(execute(&plan, false).is_err());
    assert_eq!(hash(&target).unwrap(), before);
    fs::remove_file(lock).unwrap();
    let result = execute(&plan, false).unwrap();
    assert!(result.backup.is_none());
    assert!(!fs::read_dir(root).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("backup")));
}

#[test]
fn delimited_encoding_and_quoted_text() {
    use avt_replenishment::core::read_table;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("test.txt");
    let text = "識別\t数量\r\n\"商品,名前\"\t1,185\r\n";
    let bytes: Vec<u8> = [0xff, 0xfe]
        .into_iter()
        .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    fs::write(&p, bytes).unwrap();
    let (rows, encoding) = read_table(&p, "JP").unwrap();
    assert_eq!(encoding, "UTF-16LE");
    assert_eq!(rows[1][0], Value::Text("商品,名前".into()));
    let (encoded, _, errors) = encoding_rs::SHIFT_JIS.encode(text);
    assert!(!errors);
    fs::write(&p, encoded).unwrap();
    assert_eq!(
        read_table(&p, "JP").unwrap().0[1][0],
        Value::Text("商品,名前".into())
    );
}

#[test]
fn filling_leaves_no_local_records_with_either_backup_setting() {
    use std::collections::BTreeSet;
    for backup in [false, true] {
        let (_t, target) = setup("UK");
        let root = target.parent().unwrap();
        report(
            &root.join("业务报告/20260824/AVT-UK-7T-20260824.csv"),
            15,
            "UK",
        );
        let entries = || -> BTreeSet<PathBuf> {
            fs::read_dir(root)
                .unwrap()
                .map(|e| fs::canonicalize(e.unwrap().path()).unwrap())
                .collect()
        };
        let before = entries();
        let result = execute(&inspect(&target).unwrap(), backup).unwrap();
        let after = entries();
        let added: BTreeSet<_> = after.difference(&before).cloned().collect();
        let expected: BTreeSet<_> = result.backup.into_iter().collect();
        assert_eq!(
            added, expected,
            "Only an explicitly selected XLSX backup may be created"
        );
    }
}
