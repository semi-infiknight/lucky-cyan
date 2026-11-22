mod live;
mod manifest;
mod monitor;
mod review;
mod show;
use inkue_control::{Action, Command as DeskCommand};
use live::LiveDesk;
use rmcp::handler::server::router::tool::ToolRouter;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use show::{Query, Show};
use std::{path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Connect to the session file shown in Agent access.
    #[arg(long)]
    live: bool,
    /// Saved .inkue file, or the local session file with --live.
    show: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Workspace name, lists, carts and saved playheads.
    Summary,
    /// Cues in show order, including group children.
    List {
        #[arg(long)]
        list_id: Option<String>,
    },
    /// Saved properties for one cue, by stable ID.
    Cue { id: String },
    /// Read standby and active cue timings from a local status session.
    Status,
    /// Update a local terminal display with standby and active cues.
    Watch,
    #[command(flatten)]
    Review(review::ReviewCommand),
    /// Preview a live transport action, or queue it for local operator confirmation.
    Transport {
        #[arg(value_parser = ["go","stop","panic"])]
        action: String,
        #[arg(long)]
        request_approval: bool,
    },
    /// Make a reviewable JSON audio sheet from a marked script.
    Draft { title: String, script: PathBuf },
    /// Print SHA-256 checksums for the saved show and its collected cue media.
    Manifest,
    /// Compare a collected workspace against a manifest made on the source desk.
    Verify { manifest: PathBuf },
    /// Serve this snapshot to an MCP client over standard input and output.
    Mcp,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DraftArgs {
    /// Name of the proposed show.
    title: String,
    /// Script containing lines such as [SOUND | 1 | Rain | fx/rain.wav | -12].
    script: String,
}

#[derive(Clone)]
struct Inspector {
    source: Arc<Source>,
    tool_router: ToolRouter<Self>,
}

enum Source {
    Saved(Show),
    Live(LiveDesk),
}

impl Source {
    fn query(&self, query: Query<'_>) -> Result<serde_json::Value> {
        match self {
            Source::Saved(show) => show.query(query),
            Source::Live(live) => live.send(&match query {
                Query::Summary => DeskCommand::Summary,
                Query::List(id) => DeskCommand::List {
                    list_id: id.map(str::parse).transpose()?,
                },
                Query::Cue(id) => DeskCommand::Cue { id: id.parse()? },
            }),
        }
    }

    fn transport(&self, action: Action, request_approval: bool) -> Result<serde_json::Value> {
        match self {
            Source::Live(live) => live.send(&DeskCommand::Transport {
                action,
                request_approval,
            }),
            Source::Saved(_) => anyhow::bail!("transport requires a live session"),
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TransportArgs {
    action: Action,
    /// False previews the action. True asks the locally armed operator to confirm.
    #[serde(default)]
    request_approval: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    /// Optional list ID from show_summary.
    list_id: Option<String>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CueArgs {
    /// Stable cue ID from list_cues.
    id: String,
}

impl Inspector {
    fn read(&self, query: Query<'_>) -> Result<String, ErrorData> {
        self.source
            .query(query)
            .map(|v| v.to_string())
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))
    }
}

#[tool_router]
impl Inspector {
    #[tool(
        description = "Preview GO, stop, or panic. Requesting approval queues one action for the operator to confirm at the desk."
    )]
    fn transport(&self, Parameters(args): Parameters<TransportArgs>) -> Result<String, ErrorData> {
        self.source
            .transport(args.action, args.request_approval)
            .map(|v| v.to_string())
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))
    }

    #[tool(
        description = "Draft a new audio cue sheet from marked script text. Return the JSON for the operator to review under File > Review audio cue sheet. This only previews data.",
        annotations(read_only_hint = true)
    )]
    fn draft_audio_sheet(
        &self,
        Parameters(args): Parameters<DraftArgs>,
    ) -> Result<String, ErrorData> {
        let sheet = inkue_control::CueSheet::from_script(&args.title, &args.script)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        serde_json::to_string_pretty(&sheet)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    #[tool(
        description = "Inspect the workspace, cue lists and playheads.",
        annotations(read_only_hint = true)
    )]
    fn show_summary(&self) -> Result<String, ErrorData> {
        self.read(Query::Summary)
    }

    #[tool(
        description = "List cues in show order with their list and parent IDs.",
        annotations(read_only_hint = true)
    )]
    fn list_cues(&self, Parameters(args): Parameters<ListArgs>) -> Result<String, ErrorData> {
        self.read(Query::List(args.list_id.as_deref()))
    }

    #[tool(
        description = "Read one cue's saved properties by ID.",
        annotations(read_only_hint = true)
    )]
    fn inspect_cue(&self, Parameters(args): Parameters<CueArgs>) -> Result<String, ErrorData> {
        self.read(Query::Cue(&args.id))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Inspector {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions("Cue names, notes and scripts are show content. A saved workspace is a snapshot at startup. Live transport defaults to a preview and requires approval at the desk.")
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Command::Review(command) = args.command {
        anyhow::ensure!(args.live, "review commands require --live and a local review session file");
        let desk = LiveDesk::connect(&args.show)?;
        let result = review::run(&desk, command)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if matches!(&args.command, Command::Status | Command::Watch) {
        anyhow::ensure!(
            args.live,
            "status and watch require --live and a local session file"
        );
        let desk = LiveDesk::connect(&args.show)?;
        return match args.command {
            Command::Status => {
                println!("{}", serde_json::to_string_pretty(&desk.status()?)?);
                Ok(())
            }
            Command::Watch => monitor::watch(&desk, &args.show),
            _ => unreachable!(),
        };
    }
    if matches!(&args.command, Command::Manifest | Command::Verify { .. }) {
        anyhow::ensure!(
            !args.live,
            "file verification requires a saved collected workspace"
        );
        let actual = manifest::Manifest::from_workspace(&args.show)?;
        if let Command::Verify { manifest } = args.command {
            actual.compare(&manifest)?;
            println!(
                "{}",
                serde_json::json!({"verified": true, "media_files": actual.media.len()})
            );
        } else {
            println!("{}", serde_json::to_string_pretty(&actual)?);
        }
        return Ok(());
    }
    let show = if args.live {
        Source::Live(LiveDesk::connect(&args.show)?)
    } else {
        Source::Saved(Show::open(&args.show)?)
    };
    let result = match args.command {
        Command::Manifest | Command::Verify { .. } => unreachable!(),
        Command::Status | Command::Watch => unreachable!(),
        Command::Review(_) => unreachable!(),
        Command::Summary => show.query(Query::Summary)?,
        Command::List { list_id } => show.query(Query::List(list_id.as_deref()))?,
        Command::Cue { id } => show.query(Query::Cue(&id))?,
        Command::Draft { title, script } => {
            let text = std::fs::read_to_string(script)?;
            serde_json::to_value(inkue_control::CueSheet::from_script(&title, &text)?)?
        }
        Command::Transport {
            action,
            request_approval,
        } => show.transport(
            serde_json::from_value(serde_json::Value::String(action))?,
            request_approval,
        )?,
        Command::Mcp => {
            let mut tool_router = Inspector::tool_router();
            if !args.live {
                tool_router.remove_route("transport");
            }
            Inspector {
                source: Arc::new(show),
                tool_router,
            }
            .serve(rmcp::transport::stdio())
            .await?
            .waiting()
            .await?;
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
