// Copyright (C) 2026 Posit Software, PBC. All rights reserved.

use std::path::Path;

use anyhow::anyhow;
use anyhow::Context;
use harp::RObject;
use libr::SEXP;
use rd_rds::RObject as RdsObject;
use rd_rds::RValue;

// Keep this adapter independent of R: rd-rds (from r-documentation-rs) does not
// yet provide a typed hsearch view. R still owns package discovery and ranking.
fn read_aliases(packages: &[String]) -> anyhow::Result<Vec<String>> {
    let mut entries = Vec::new();
    for package in packages {
        let path = Path::new(package).join("Meta/hsearch.rds");
        if !path.try_exists()? {
            continue;
        }
        let object = rd_rds::file::read(&path)
            .with_context(|| format!("Reading Help aliases from {}", path.display()))?;
        if matches!(object.value(), RValue::Null) {
            continue;
        }
        entries.extend(decode_aliases(&object)?);
    }
    Ok(entries)
}

fn decode_aliases(object: &RdsObject) -> anyhow::Result<Vec<String>> {
    let invalid = || anyhow!("Unsupported hsearch alias metadata");
    let RValue::List(tables) = object.value() else {
        return Err(invalid());
    };
    let aliases = tables.get(1).ok_or_else(invalid)?;
    let RValue::Character(values) = aliases.value() else {
        return Err(invalid());
    };
    let dimensions = aliases.attributes().get("dim").ok_or_else(invalid)?;
    let RValue::Integer(dimensions) = dimensions.value() else {
        return Err(invalid());
    };
    let [Some(rows), Some(3)] = dimensions.as_slice() else {
        return Err(invalid());
    };
    let rows = usize::try_from(*rows)?;
    if rows.checked_mul(3) != Some(values.len()) {
        return Err(invalid());
    }
    // R's installed hsearch alias table has column-major Alias, ID, Package.
    let mut entries = Vec::with_capacity(rows);
    for row in 0..rows {
        let alias = values[row].as_str().ok_or_else(invalid)??;
        let package = values[2 * rows + row].as_str().ok_or_else(invalid)??;
        if !alias.is_empty() {
            entries.push(format!("{package}\u{1f}{alias}"));
        }
    }
    Ok(entries)
}

#[harp::register]
pub unsafe extern "C-unwind" fn ps_help_read_aliases(packages: SEXP) -> anyhow::Result<SEXP> {
    let packages = RObject::view(packages).to::<Vec<String>>()?;
    Ok(RObject::from(read_aliases(&packages)?).sexp)
}
