use clap::Parser;
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;
use sysone::{Client, Request};

#[derive(Debug, Parser)]
#[command(
    name = "findme",
    about = "Find a file or folder from a natural language description"
)]
struct Arguments {
    /// What you remember about the file or folder.
    query: String,

    /// Directory where the search should start.
    #[arg(short, long, default_value = ".")]
    root: PathBuf,

    /// Maximum number of directory levels to inspect.
    #[arg(long, default_value_t = 8)]
    max_depth: usize,

    /// Number of promising directories to keep at each level.
    #[arg(long, default_value_t = 8)]
    beam_width: usize,

    /// Number of results to print.
    #[arg(short, long, default_value_t = 10)]
    results: usize,

    /// Disable searching hidden files and directories.
    #[arg(long)]
    no_hidden: bool,

    /// Print each folder and entry as it is inspected.
    #[arg(long)]
    follow: bool,

    /// Show results below the default 50% confidence threshold.
    #[arg(long)]
    all_results: bool,

    /// Disable searching up to four parent directories.
    #[arg(long)]
    no_parent_fallback: bool,
}

#[derive(Clone)]
struct Candidate {
    path: PathBuf,
    parent_path: PathBuf,
    description: String,
    is_directory: bool,
    score: f64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let search_started_at = Instant::now();
    let root = fs::canonicalize(&arguments.root)?;

    if !root.is_dir() {
        return Err(format!("search root is not a directory: {}", root.display()).into());
    }
    if std::env::var("TYPESAFE_API_KEY").map_or(true, |key| key.trim().is_empty()) {
        return Err("TYPESAFE_API_KEY is not set".into());
    }
    if arguments.beam_width == 0 || arguments.results == 0 {
        return Err("--beam-width and --results must be greater than zero".into());
    }

    let client = Client::default();
    let mut directories = vec![Candidate {
        path: root.clone(),
        parent_path: root.clone(),
        description: describe_path(&root, true),
        is_directory: true,
        score: 1.0,
    }];
    if !arguments.no_parent_fallback {
        let mut parent = root.parent().map(Path::to_path_buf);
        for _ in 0..4 {
            let Some(path) = parent else {
                break;
            };
            if path.parent().is_none() || path.parent().is_some_and(|parent| parent == path) {
                break;
            }
            directories.push(Candidate {
                description: describe_path(&path, true),
                path: path.clone(),
                parent_path: path.clone(),
                is_directory: true,
                score: 1.0,
            });
            let next_parent = path.parent().map(Path::to_path_buf);
            if next_parent.as_deref() == Some(path.as_path()) {
                break;
            }
            parent = next_parent;
        }
    }
    let mut results = Vec::new();

    for search_depth in 0..arguments.max_depth {
        let children: Vec<Candidate> = directories
            .par_iter()
            .flat_map_iter(|directory| {
                list_children(directory, !arguments.no_hidden, arguments.follow)
            })
            .collect();
        if children.is_empty() {
            break;
        }

        let spinner = create_spinner(&root);
        let ranked_result = rank_candidates(&client, &arguments.query, children).await;
        spinner.finish_and_clear();
        let ranked = ranked_result?;

        let mut ranked_directories = Vec::new();
        for candidate in ranked {
            if candidate.is_directory {
                ranked_directories.push(candidate.clone());
            }
            results.push(candidate);
        }
        if search_depth == 0 {
            let mut directories_by_parent: HashMap<PathBuf, Vec<Candidate>> = HashMap::new();
            for candidate in ranked_directories {
                directories_by_parent
                    .entry(candidate.parent_path.clone())
                    .or_default()
                    .push(candidate);
            }
            directories = directories_by_parent
                .into_values()
                .flat_map(|mut candidates| {
                    candidates.sort_by(compare_candidates);
                    candidates.truncate(1);
                    candidates
                })
                .collect();
        } else {
            ranked_directories.sort_by(compare_candidates);
            ranked_directories.truncate(arguments.beam_width);
            directories = ranked_directories;
        }
        if directories.is_empty() {
            break;
        }
    }

    let mut unique_results = HashMap::new();
    for result in results {
        unique_results
            .entry(result.path.clone())
            .and_modify(|existing: &mut Candidate| {
                if result.score > existing.score {
                    *existing = result.clone();
                }
            })
            .or_insert(result);
    }
    let mut results: Vec<Candidate> = unique_results.into_values().collect();

    if !results.is_empty() {
        results.sort_by(compare_candidates);
        results.truncate(50);
        for result in &mut results {
            result.score = 1.0;
        }

        let spinner = create_spinner(&root);
        let reranked_result = rank_candidates(&client, &arguments.query, results).await;
        spinner.finish_and_clear();
        results = reranked_result?;
    }

    results.sort_by(compare_candidates);
    if !arguments.all_results {
        if let Some(best_score) = results.first().map(|result| result.score) {
            let minimum_score = best_score * 0.1;
            results.retain(|result| result.score > 0.0 && result.score >= minimum_score);
        }
    }
    results.truncate(arguments.results);
    if results.is_empty() {
        println!("No files found in the inspected paths.");
    } else {
        println!("\nMost likely paths:");
        for (index, result) in results.iter().enumerate() {
            println!(
                "{}. {:>5.1}%  {}",
                index + 1,
                result.score * 100.0,
                result.path.display()
            );
        }
    }
    println!(
        "\nCompleted in {:.2}s",
        search_started_at.elapsed().as_secs_f64()
    );
    Ok(())
}

fn create_spinner(path: &Path) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    let style = ProgressStyle::with_template("{spinner:.yellow} Sniffing around {msg}...")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);
    spinner.set_style(style);
    spinner.set_message(path.display().to_string());
    spinner.enable_steady_tick(std::time::Duration::from_millis(90));
    spinner
}

fn list_children(directory: &Candidate, include_hidden: bool, follow: bool) -> Vec<Candidate> {
    if follow {
        println!("Walking folder {}", directory.path.display());
    }

    let entries: Vec<_> = WalkBuilder::new(&directory.path)
        .max_depth(Some(1))
        .standard_filters(true)
        .hidden(!include_hidden)
        .build()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.depth() == 1 => {
                let file_type = entry.file_type()?;
                if matches!(
                    entry.file_name().to_str(),
                    Some(".git" | ".cache" | ".local" | ".cargo")
                ) || file_type.is_symlink()
                {
                    return None;
                }
                Some((entry.path().to_path_buf(), file_type.is_dir()))
            }
            _ => None,
        })
        .collect();

    entries
        .par_iter()
        .map(|(path, is_directory)| {
            if follow {
                println!("  Walking {}", path.display());
            }
            Candidate {
                description: describe_path(path, *is_directory),
                path: path.clone(),
                parent_path: directory.path.clone(),
                is_directory: *is_directory,
                score: directory.score,
            }
        })
        .collect()
}

fn describe_path(path: &Path, is_directory: bool) -> String {
    let kind = if is_directory { "directory" } else { "file" };
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut description = format!("{kind} named {name}, path {}", path.display());

    if !is_directory {
        if let Ok(metadata) = fs::metadata(path) {
            description.push_str(&format!(", {} bytes", metadata.len()));
        }
    }
    for metadata_file in ["README.md", "Cargo.toml", "package.json"] {
        let metadata_path = if is_directory {
            path.join(metadata_file)
        } else {
            continue;
        };
        if let Some(excerpt) = read_excerpt(&metadata_path) {
            description.push_str(&format!(", {metadata_file} excerpt: {excerpt}"));
            break;
        }
    }
    description
}

fn read_excerpt(path: &Path) -> Option<String> {
    let mut buffer = [0; 240];
    let bytes_read = fs::File::open(path)
        .ok()?
        .take(buffer.len() as u64)
        .read(&mut buffer)
        .ok()?;
    Some(String::from_utf8_lossy(&buffer[..bytes_read]).replace('\n', " "))
}

async fn rank_candidates(
    client: &Client,
    query: &str,
    candidates: Vec<Candidate>,
) -> Result<Vec<Candidate>, Box<dyn std::error::Error>> {
    let mut ranked = Vec::with_capacity(candidates.len());
    for chunk in candidates.chunks(50) {
        let mut criteria = serde_json::Map::new();
        for (index, candidate) in chunk.iter().enumerate() {
            criteria.insert(
                format!("candidate_{index}"),
                Value::String(candidate.description.clone()),
            );
        }
        let state = json!({
            "search_request": query,
        });
        let request = Request::from_state(state).append_question("best_match", json!({
            "type": "choice",
            "instructions": format!("Which candidate is most likely to be the file or folder described by: {query}"),
            "criteria": criteria,
        }));
        let response = client.exec(request).await?;
        let probabilities = extract_probabilities(&response);
        for (index, mut candidate) in chunk.iter().cloned().enumerate() {
            let key = format!("candidate_{index}");
            let relevance = probabilities.get(&key).copied().unwrap_or(0.0);
            candidate.score *= relevance.max(0.000001);
            ranked.push(candidate);
        }
    }
    ranked.sort_by(compare_candidates);
    Ok(ranked)
}

fn extract_probabilities(response: &Value) -> HashMap<String, f64> {
    let answers = response
        .get("answers")
        .or_else(|| response.get("answer"))
        .unwrap_or(response);
    let answer = answers.get("best_match").unwrap_or(answers);
    let probabilities = answer.get("probabilities").unwrap_or(answer);
    probabilities
        .as_object()
        .map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| value.as_f64().map(|score| (key.clone(), score)))
                .collect()
        })
        .unwrap_or_default()
}

fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
}
