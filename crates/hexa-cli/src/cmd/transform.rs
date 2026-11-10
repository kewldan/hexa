//! `transform`: run a `hexa-transform` recipe over a file or a range.

use std::path::PathBuf;

use clap::Args;
use hexa_transform::{MAX_OUTPUT_LEN, Recipe};
use serde_json::json;

use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, RangeArgs, Sink, open_tree, read_text};

#[derive(Debug, Args)]
pub struct TransformArgs {
    /// Input file (`-` = stdin).
    pub file: PathBuf,
    /// Recipe: a JSON file, or inline JSON starting with `{` / `[`
    /// (`{"steps":[{"kind":"bitwise","op":"xor","operand":255}]}` or a bare
    /// step array).
    #[arg(short, long, value_name = "FILE|JSON")]
    pub recipe: String,
    /// Run the inverse recipe (every step must be reversible).
    #[arg(long)]
    pub reverse: bool,
    /// Output file (default: stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Write the whole file with only the range replaced (default: only the
    /// transformed range).
    #[arg(long, requires = "start")]
    pub splice: bool,
    #[command(flatten)]
    pub range: RangeArgs,
}

fn load_recipe(spec: &str) -> CliResult<Recipe> {
    let trimmed = spec.trim_start();
    let text = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        spec.to_owned()
    } else {
        read_text(std::path::Path::new(spec))?
    };
    let recipe = if text.trim_start().starts_with('[') {
        Recipe::new(
            serde_json::from_str(&text).map_err(|e| CliError::data(format!("recipe: {e}")))?,
        )
    } else {
        Recipe::from_json(&text).map_err(|e| CliError::data(format!("recipe: {e}")))?
    };
    recipe
        .validate()
        .map_err(|e| CliError::data(format!("recipe: {e}")))?;
    Ok(recipe)
}

pub fn run(a: &TransformArgs, json_out: bool) -> CliResult {
    let mut recipe = load_recipe(&a.recipe)?;
    if a.reverse {
        recipe = recipe.invert()?;
    }
    let tree = open_tree(&a.file)?;
    let (start, end) = a.range.resolve(tree.len())?;
    let data = io::tree_bytes(&tree, start, end, MAX_OUTPUT_LEN as u64)?;
    let out = hexa_transform::apply(&recipe, &data)?;
    let to_stdout = a.output.as_deref().is_none_or(io::is_std);
    if to_stdout && json_out {
        return Err(CliError::usage(
            "--json needs --output for the transformed data",
        ));
    }
    let mut sink = Sink::create(a.output.as_deref())?;
    if a.splice {
        sink.write_tree(&tree, 0, start)?;
        sink.write_all(&out)?;
        sink.write_tree(&tree, end, tree.len())?;
    } else {
        sink.write_all(&out)?;
    }
    sink.finish()?;
    if json_out {
        io::print_json(&json!({
            "steps": recipe.steps.len(),
            "inputLen": end - start,
            "outputLen": out.len(),
        }))?;
    }
    Ok(Outcome::Ok)
}
