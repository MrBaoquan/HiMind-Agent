use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::error::Error;
use std::time::Instant;

use crate::api::types::Task;
use crate::scan::service::normalize_scan_text;
use crate::{report_task, Options};

use super::client::{
    extract_between, extract_table_cells, extract_table_header_cells, inner_admin_base,
    inner_admin_client, inner_admin_login, is_login_page,
};

#[derive(Debug, Clone)]
struct EngineeringWorkload {
    developer: String,
    workload: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct EngineeringProjectSummary {
    pub id: String,
    pub project_name: String,
    pub owner: String,
    pub exhibit_count: usize,
    pub source_type: String,
    pub source_key: String,
    pub source_name: String,
}

pub(crate) fn execute_sync_exhibits(
    dashboard_client: &Client,
    options: &Options,
    agent_id: &str,
    task: &Task,
) -> Result<Value, Box<dyn Error>> {
    let base = inner_admin_base();
    let client = inner_admin_client()?;
    let started = Instant::now();
    inner_admin_login(&client, &base, !options.local_app)?;
    report_task(
        dashboard_client,
        options,
        agent_id,
        &task.id,
        "running",
        22,
        "内网登录成功，读取个人待上传展项",
        None,
        None,
    )?;
    let mut exhibits = Vec::new();
    let mut pages_read = 0;
    for page in 1..=100 {
        pages_read = page;
        let progress = std::cmp::min(60, 24 + page * 3);
        report_task(
            dashboard_client,
            options,
            agent_id,
            &task.id,
            "running",
            progress,
            &format!("读取未上传展项第 {} 页，已同步 {} 个", page, exhibits.len()),
            None,
            None,
        )?;
        let url = if page == 1 {
            format!("{}/admin/personal/software_code", base)
        } else {
            format!("{}/admin/personal/software_code?page={}", base, page)
        };
        let html = client.get(&url).send()?.error_for_status()?.text()?;
        if is_login_page(&html) {
            return Err("inner admin login expired while syncing exhibits".into());
        }
        let page_items = parse_software_code_page(&base, &html);
        if page_items.is_empty() {
            break;
        }
        report_task(
            dashboard_client,
            options,
            agent_id,
            &task.id,
            "running",
            progress,
            &format!(
                "第 {} 页完成：累计 {} 个待上传展项",
                page,
                exhibits.len() + page_items.len()
            ),
            None,
            None,
        )?;
        exhibits.extend(page_items);
        if !html.contains(&format!("software_code?page={}", page + 1)) {
            break;
        }
    }
    Ok(json!({
        "stage": "synced",
        "count": exhibits.len(),
        "exhibits": exhibits,
        "pages": pages_read,
        "elapsed_ms": started.elapsed().as_millis(),
    }))
}

pub(crate) fn fetch_engineering_projects() -> Result<Vec<EngineeringProjectSummary>, Box<dyn Error>>
{
    let base = inner_admin_base();
    let client = inner_admin_client()?;
    inner_admin_login(&client, &base, true)?;
    fetch_engineering_projects_with_client(&client, &base)
}

pub(crate) fn fetch_selected_engineering_exhibits(
    project_ids: &[String],
) -> Result<Vec<Value>, Box<dyn Error>> {
    let base = inner_admin_base();
    let client = inner_admin_client()?;
    inner_admin_login(&client, &base, true)?;
    let projects = fetch_engineering_projects_with_client(&client, &base)?;
    let project_index: HashMap<String, EngineeringProjectSummary> = projects
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect();
    let mut workload_cache: HashMap<String, HashMap<String, Vec<EngineeringWorkload>>> =
        HashMap::new();
    let mut exhibits = Vec::new();
    for project_id in project_ids {
        let project_id = project_id.trim();
        if project_id.is_empty() {
            continue;
        }
        let Some(project) = project_index.get(project_id) else {
            continue;
        };
        if !workload_cache.contains_key(project_id) {
            if let Ok(workloads) = fetch_engineering_workloads(&client, &base, project_id) {
                workload_cache.insert(project_id.to_string(), workloads);
            }
        }
        let workloads = workload_cache.get(project_id);
        exhibits.extend(fetch_engineering_exhibits_pages(
            &client, &base, project, workloads,
        )?);
    }
    Ok(exhibits)
}

fn parse_software_code_page(base: &str, html: &str) -> Vec<Value> {
    let mut items = Vec::new();
    for row in html.split("<tr").skip(1) {
        if !row.contains("/admin/software/product/") || !row.contains("去上传") {
            continue;
        }
        let Some(pid) = extract_between(row, "/admin/software/product/", "/edit") else {
            continue;
        };
        let cells = extract_table_cells(row);
        if cells.len() < 3 {
            continue;
        }
        items.push(json!({
            "pid": pid,
            "exhibit_name": cells[1],
            "project_name": cells[2],
            "edit_url": format!("{}/admin/software/product/{}/edit", base, pid),
            "status": "unuploaded",
            "source_type": "inner_admin_pending",
            "source_key": pid,
            "source_name": cells[1],
            "source_scope": "personal/software_code",
        }));
    }
    items
}

fn fetch_engineering_projects_with_client(
    client: &Client,
    base: &str,
) -> Result<Vec<EngineeringProjectSummary>, Box<dyn Error>> {
    let mut items = Vec::new();
    let mut seen = HashMap::new();
    for page in 1..=30 {
        let url = if page == 1 {
            format!("{}/admin/software/engineering", base)
        } else {
            format!("{}/admin/software/engineering?page={}", base, page)
        };
        let html = client.get(&url).send()?.error_for_status()?.text()?;
        if is_login_page(&html) {
            return Err("inner admin login expired while reading engineering list".into());
        }
        let owner_column = engineering_owner_column_index(&extract_table_header_cells(&html));
        let mut page_count = 0;
        for row in html.split("<tr").skip(1) {
            let Some(id) = extract_between(row, "/admin/software/engineering/", "\"")
                .or_else(|| extract_between(row, "/admin/software/engineering/", "'"))
            else {
                continue;
            };
            let cells = extract_table_cells(row);
            if cells.len() < 2 || id.contains('?') || id.contains('#') {
                continue;
            }
            let exhibit_count = cells
                .iter()
                .rev()
                .find_map(|cell| cell.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let project_name = cells[1].trim().to_string();
            seen.insert(
                id.clone(),
                EngineeringProjectSummary {
                    id: id.clone(),
                    project_name: project_name.clone(),
                    owner: owner_column
                        .and_then(|index| cells.get(index).map(|cell| cell.trim().to_string()))
                        .filter(|value| is_valid_engineering_owner(value))
                        .unwrap_or_else(|| extract_engineering_owner(&cells)),
                    exhibit_count,
                    source_type: "inner_admin".to_string(),
                    source_key: id,
                    source_name: project_name,
                },
            );
            page_count += 1;
        }
        if page_count == 0 || !html.contains(&format!("engineering?page={}", page + 1)) {
            break;
        }
    }
    items.extend(seen.into_values());
    items.sort_by(|left, right| compare_engineering_project_id_desc(&left.id, &right.id));
    Ok(items)
}

fn compare_engineering_project_id_desc(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left_id), Ok(right_id)) => right_id.cmp(&left_id),
        _ => right.cmp(left),
    }
}

fn fetch_engineering_exhibits_pages(
    client: &Client,
    base: &str,
    project: &EngineeringProjectSummary,
    workloads: Option<&HashMap<String, Vec<EngineeringWorkload>>>,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let mut items = Vec::new();
    for page in 1..=30 {
        let url = if page == 1 {
            format!("{}/admin/software/engineering/{}", base, project.id)
        } else {
            format!(
                "{}/admin/software/engineering/{}?page={}",
                base, project.id, page
            )
        };
        let html = client.get(&url).send()?.error_for_status()?.text()?;
        if is_login_page(&html) {
            return Err("inner admin login expired while reading engineering exhibits".into());
        }
        let mut page_items = parse_engineering_exhibits(&html, base, project, workloads);
        let page_count = page_items.len();
        items.append(&mut page_items);
        if page_count == 0
            || !html.contains(&format!("engineering/{}?page={}", project.id, page + 1))
        {
            break;
        }
    }
    Ok(items)
}

fn parse_engineering_exhibits(
    html: &str,
    base: &str,
    project: &EngineeringProjectSummary,
    workloads: Option<&HashMap<String, Vec<EngineeringWorkload>>>,
) -> Vec<Value> {
    let mut items = Vec::new();
    let columns = engineering_table_columns(&extract_table_header_cells(html));
    for row in html.split("<tr").skip(1) {
        if !row.contains("/admin/software/product/") {
            continue;
        }
        let cells = extract_table_cells(row);
        let exhibit_name = cells
            .get(columns.exhibit_index)
            .map(|cell| cell.trim().to_string())
            .unwrap_or_default();
        if exhibit_name.is_empty() {
            continue;
        }
        let Some(pid) = extract_product_id(row) else {
            continue;
        };
        let mut item = json!({
            "pid": pid,
            "exhibit_name": exhibit_name,
            "project_name": project.project_name,
            "project_owner": project.owner,
            "edit_url": format!("{}/admin/software/product/{}/edit", base, pid),
            "engineering_id": project.id,
            "developer_source": format!("{}/admin/software/engineering/{}", base, project.id),
            "status": "synced",
            "source_type": "inner_admin",
            "source_key": format!("{}:{}", project.id, pid),
            "source_name": exhibit_name,
            "source_scope": project.id,
        });
        if let Some(hall_index) = columns.hall_index {
            if let Some(hall) = cells
                .get(hall_index)
                .map(|cell| cell.trim())
                .filter(|cell| !cell.is_empty())
            {
                item["hall"] = json!(hall);
            }
        }
        if let Some(matched) =
            workloads.and_then(|value| find_engineering_workload(&pid, &exhibit_name, value))
        {
            let developer_text = primary_engineering_developer(matched).unwrap_or_default();
            let workload_total: f64 = matched.iter().map(|entry| entry.workload).sum();
            item["developer"] = json!(developer_text);
            item["workload"] = json!(workload_total);
            item["developer_workloads"] = json!(matched
                .iter()
                .map(|entry| json!({ "developer": entry.developer, "workload": entry.workload }))
                .collect::<Vec<_>>());
        }
        items.push(item);
    }
    items
}

fn extract_product_id(row: &str) -> Option<String> {
    let marker = "/admin/software/product/";
    let start = row.find(marker)? + marker.len();
    let value = row[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect::<String>();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn fetch_engineering_workloads(
    client: &Client,
    base: &str,
    engineering_id: &str,
) -> Result<HashMap<String, Vec<EngineeringWorkload>>, Box<dyn Error>> {
    let mut workloads = HashMap::new();
    for page in 1..=30 {
        let url = if page == 1 {
            format!("{}/admin/software/engineering/{}", base, engineering_id)
        } else {
            format!(
                "{}/admin/software/engineering/{}?page={}",
                base, engineering_id, page
            )
        };
        let html = client.get(&url).send()?.error_for_status()?.text()?;
        if is_login_page(&html) {
            return Err("inner admin login expired while reading engineering workloads".into());
        }
        let page_workloads = parse_engineering_workloads(&html);
        let mut page_count = 0;
        for (key, developers) in page_workloads {
            workloads.insert(key, developers);
            page_count += 1;
        }
        if page_count == 0
            || !html.contains(&format!("engineering/{}?page={}", engineering_id, page + 1))
        {
            break;
        }
    }
    Ok(workloads)
}

fn parse_engineering_workloads(html: &str) -> HashMap<String, Vec<EngineeringWorkload>> {
    let mut workloads = HashMap::new();
    let columns = engineering_table_columns(&extract_table_header_cells(html));
    for row in html.split("<tr").skip(1) {
        if !row.contains("/admin/software/product/") {
            continue;
        }
        let cells = extract_table_cells(row);
        let exhibit_name = cells
            .get(columns.exhibit_index)
            .map(|cell| normalize_scan_text(cell))
            .unwrap_or_default();
        if exhibit_name.is_empty() {
            continue;
        }
        let workload = columns
            .workload_index
            .and_then(|index| cells.get(index))
            .and_then(|value| parse_engineering_workload_value(value))
            .unwrap_or(1.0);
        let developer_text = columns
            .developer_index
            .and_then(|index| cells.get(index))
            .map(String::as_str)
            .unwrap_or("");
        let developers = parse_developer_workload_text(developer_text, workload);
        if !developers.is_empty() {
            if let Some(pid) = extract_between(row, "/admin/software/product/", "/")
                .or_else(|| extract_between(row, "/admin/software/product/", "\""))
                .or_else(|| extract_between(row, "/admin/software/product/", "'"))
            {
                workloads.insert(format!("pid:{}", pid.trim()), developers.clone());
            }
            workloads.insert(exhibit_name, developers);
        }
    }
    workloads
}

fn find_engineering_workload<'a>(
    pid: &str,
    exhibit_name: &str,
    workloads: &'a HashMap<String, Vec<EngineeringWorkload>>,
) -> Option<&'a Vec<EngineeringWorkload>> {
    if !pid.trim().is_empty() {
        if let Some(values) = workloads.get(&format!("pid:{}", pid.trim())) {
            return Some(values);
        }
    }
    let target = normalize_scan_text(exhibit_name);
    workloads.get(&target).or_else(|| {
        workloads.iter().find_map(|(name, values)| {
            if name.starts_with("pid:") {
                return None;
            }
            if !target.is_empty() && (target.contains(name) || name.contains(&target)) {
                Some(values)
            } else {
                None
            }
        })
    })
}

fn primary_engineering_developer(workloads: &[EngineeringWorkload]) -> Option<String> {
    workloads
        .iter()
        .max_by(|left, right| left.workload.total_cmp(&right.workload))
        .map(|item| item.developer.clone())
}

fn extract_engineering_owner(cells: &[String]) -> String {
    cells
        .iter()
        .skip(2)
        .map(|cell| cell.trim())
        .find(|cell| is_valid_engineering_owner(cell))
        .unwrap_or("")
        .to_string()
}

fn engineering_owner_column_index(headers: &[String]) -> Option<usize> {
    headers.iter().position(|header| {
        let normalized = header.trim().replace(' ', "");
        normalized.contains("软件负责人")
            || normalized.contains("项目负责人")
            || normalized == "负责人"
    })
}

fn is_valid_engineering_owner(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.parse::<usize>().is_err()
        && !matches!(trimmed, "查看" | "编辑" | "删除" | "详情" | "状态" | "操作")
        && !trimmed.contains(':')
}

fn parse_developer_workload_text(text: &str, workload: f64) -> Vec<EngineeringWorkload> {
    let parts: Vec<String> = text
        .split(';')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect();
    if parts.is_empty() {
        let developer = text.trim();
        if developer.is_empty() {
            return Vec::new();
        }
        return vec![EngineeringWorkload {
            developer: developer.to_string(),
            workload,
        }];
    }
    let mut out = Vec::new();
    for part in &parts {
        let developer = part.split('(').next().unwrap_or(part).trim();
        if developer.is_empty() {
            continue;
        }
        let percent = extract_between(part, "(", "%)")
            .and_then(|value| value.trim().parse::<f64>().ok())
            .unwrap_or_else(|| 100.0 / parts.len() as f64);
        out.push(EngineeringWorkload {
            developer: developer.to_string(),
            workload: workload * percent / 100.0,
        });
    }
    out
}

#[derive(Clone, Copy)]
struct EngineeringTableColumns {
    hall_index: Option<usize>,
    exhibit_index: usize,
    developer_index: Option<usize>,
    workload_index: Option<usize>,
}

fn engineering_table_columns(headers: &[String]) -> EngineeringTableColumns {
    EngineeringTableColumns {
        hall_index: find_engineering_header_index(
            headers,
            &["所属展厅", "归属展厅", "展厅", "展区", "区域"],
        ),
        exhibit_index: find_engineering_header_index(
            headers,
            &["展项名称", "展项", "软件名称", "名称"],
        )
        .unwrap_or(3),
        developer_index: find_engineering_header_index(
            headers,
            &["制作人员", "开发人员", "软件负责人", "负责人"],
        ),
        workload_index: find_engineering_header_index(headers, &["工作量", "占比", "制作占比"]),
    }
}

fn find_engineering_header_index(headers: &[String], candidates: &[&str]) -> Option<usize> {
    headers.iter().position(|header| {
        let normalized = header.trim().replace(' ', "");
        candidates
            .iter()
            .any(|candidate| normalized.contains(candidate))
    })
}

fn parse_engineering_workload_value(value: &str) -> Option<f64> {
    let normalized = value.trim().trim_end_matches('%').replace(',', "");
    normalized.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::extract_product_id;

    #[test]
    fn extracts_product_id_without_html_link_text() {
        let row = r#"<a href="/admin/software/product/9680">查看</a>"#;
        assert_eq!(extract_product_id(row).as_deref(), Some("9680"));
    }
}
