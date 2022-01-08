//! The helper buildkit front-end mentioned in `buildkit.rs`.
#![allow(dead_code)]
// (otherwise there will be a lot of warnings for functions that are only used in the main binary.)

mod buildkit;
mod builtin;
mod dockerfile;
mod imagegen;
mod logic;
mod modusfile;
mod reporting;
mod registry;
mod sld;
mod translate;
mod transpiler;
mod unification;
mod wellformed;

mod buildkit_llb_types;
use buildkit_llb_types::OwnedOutput;

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};

use buildkit_frontend::{
    oci::{Architecture, ImageConfig, ImageSpecification, OperatingSystem},
    run_frontend, Bridge, Frontend, FrontendOutput,
};
use buildkit_llb::prelude::*;
use buildkit_llb::prelude::{fs::CopyOperation, source::LocalSource};

use async_trait::async_trait;

use imagegen::{BuildNode, BuildPlan, NodeId};

#[macro_use]
extern crate serde;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    run_frontend(TheFrontend).await?;
    Ok(())
}

struct TheFrontend;

#[derive(Deserialize)]
struct FrontendOptions {
    filename: String,
    target: Option<String>,
    has_dockerignore: bool,
    #[serde(flatten)]
    others: HashMap<String, serde_json::Value>,
}

#[async_trait]
impl Frontend<FrontendOptions> for TheFrontend {
    async fn run(
        self,
        bridge: Bridge,
        options: FrontendOptions,
    ) -> Result<FrontendOutput, failure::Error> {
        let build_plan = fetch_input(&bridge, &options).await;
        let mut outputs = handle_build_plan(&bridge, &options, &build_plan).await;
        let final_output;
        if outputs.len() == 1 {
            final_output = outputs.into_iter().next().unwrap();
        } else if options.target.is_some() && !options.target.as_ref().unwrap().is_empty() {
            let target_idx: usize = options
                .target
                .as_ref()
                .unwrap()
                .parse()
                .expect("Expected target to be an usize");
            final_output = outputs.swap_remove(target_idx);
        } else {
            let alpine = Source::image("alpine")
                .custom_name("Getting an alpine image as a stub for the final image")
                .ref_counted();
            let (_, alpine_config) = bridge
                .resolve_image_config(&alpine, Some("alpine (stub) :: resolve"))
                .await?;
            let mut command = Command::run("true").cwd("/").mount(Mount::Layer(
                OutputIdx(0),
                SingleOwnedOutput::output(&alpine),
                "/",
            ));
            let mut idx = 1usize;
            for o in &outputs {
                command = command.mount(Mount::Layer(
                    OutputIdx(idx as u32),
                    o.0.output(),
                    format!("/_{}", idx),
                ));
                idx += 1;
            }
            command = command.custom_name("Finishing multiple output images");
            final_output = (
                OwnedOutput::from_command(command.ref_counted(), 0),
                Arc::new(alpine_config),
            );
        }
        let solved = bridge
            .solve(Terminal::with(final_output.0.output()))
            .await
            .expect("Unable to solve");
        Ok(FrontendOutput::with_spec_and_ref(
            (*final_output.1).clone(),
            solved,
        ))
    }
}

async fn read_local_file(bridge: &Bridge, filename: &str) -> Vec<u8> {
    let mut local_source = Source::local("context").custom_name(format!("Reading {}", filename));
    local_source = local_source.add_include_pattern(filename);
    let local_output = local_source.output();
    let local_ref = bridge
        .solve(Terminal::with(local_output))
        .await
        .expect("Failed to get local context");
    let input = bridge
        .read_file(&local_ref, filename, None)
        .await
        .expect("Failed to read local file");
    input
}

async fn fetch_input(bridge: &Bridge, options: &FrontendOptions) -> BuildPlan {
    let input_filename = &options.filename;
    let input_file_bytes = read_local_file(bridge, input_filename).await;
    let input_file_content =
        std::str::from_utf8(&input_file_bytes[..]).expect("Expected input to be UTF8");
    let start = input_file_content.find('\n').expect("Invalid input") + 1;
    serde_json::from_slice(&input_file_bytes[start..]).expect("Invalid input")
}

async fn handle_build_plan(
    bridge: &Bridge,
    options: &FrontendOptions,
    build_plan: &BuildPlan,
) -> Vec<(OwnedOutput, Arc<ImageSpecification>)> {
    let mut translated_nodes: Vec<Option<(OwnedOutput, Arc<ImageSpecification>)>> =
        Vec::with_capacity(build_plan.nodes.len());
    for _ in 0..build_plan.nodes.len() {
        // Need to push in a loop since type is not cloneable.
        translated_nodes.push(None);
    }

    fn get_cwd_from_image_spec(image_spec: &ImageSpecification) -> PathBuf {
        image_spec
            .config
            .as_ref()
            .and_then(|x| x.working_dir.clone())
            .map(|x| {
                if !x.has_root() {
                    PathBuf::from("/").join(x)
                } else {
                    x
                }
            })
            .unwrap_or_else(|| PathBuf::from("/"))
    }
    fn empty_image_config() -> ImageConfig {
        ImageConfig {
            user: None,
            exposed_ports: None,
            env: None,
            entrypoint: None,
            cmd: None,
            volumes: None,
            working_dir: None,
            labels: None,
            stop_signal: None,
        }
    }

    async fn get_local_source_for_copy(
        bridge: &Bridge,
        should_read_ignore_file: bool,
    ) -> OperationOutput<'static> {
        let mut source = Source::local("context").custom_name("Sending local context for copy");
        if should_read_ignore_file {
            let dockerignore_bytes = read_local_file(bridge, ".dockerignore").await;
            let dockerignore = std::str::from_utf8(&dockerignore_bytes)
                .expect("Expected .dockerignore to contain valid utf-8 content.");
            for line in dockerignore.lines() {
                source = source.add_exclude_pattern(line);
            }
        }
        source = source.add_exclude_pattern(buildkit::TMP_PREFIX_IGNORE_PATTERN);
        source.ref_counted().output()
    }

    let local_context = get_local_source_for_copy(bridge, options.has_dockerignore).await;

    for node_id in build_plan.topological_order().into_iter() {
        let node = &build_plan.nodes[node_id];
        use BuildNode::*;
        let new_node: (OwnedOutput, Arc<ImageSpecification>) = match node {
            From { image_ref } => {
                let img_s = Source::image(image_ref).custom_name(format!("from({:?})", image_ref));
                let log_name = format!("from({:?}) :: resolve image config", image_ref);
                let (_, resolved_config) = bridge
                    .resolve_image_config(&img_s, Some(&log_name))
                    .await
                    .expect("Resolution failed.");
                (img_s.ref_counted().into(), Arc::new(resolved_config))
            }
            Run {
                parent,
                command,
                cwd,
            } => {
                let parent = translated_nodes[*parent]
                    .as_ref()
                    .expect("Expected dependencies to already be built");
                let parent_config = parent.1.clone();
                let user = parent_config
                    .config
                    .as_ref()
                    .and_then(|x| x.user.as_ref().map(|x| &x[..]))
                    .unwrap_or("0");
                let mut cmd = Command::run("sh") // TDDO: use image shell config
                    .args(&["-c", &command[..]])
                    .custom_name(format!("run({:?})", command))
                    .cwd(get_cwd_from_image_spec(&parent_config).join(cwd))
                    .user(user)
                    .mount(Mount::Layer(OutputIdx(0), parent.0.output(), "/"));
                let envs = parent_config.config.as_ref().and_then(|x| x.env.as_ref());
                if let Some(env_map) = envs {
                    for (key, value) in env_map.iter() {
                        cmd = cmd.env(key, value);
                    }
                }
                let o = OwnedOutput::from_command(cmd.ref_counted(), 0);
                (o, parent_config)
            }
            CopyFromImage {
                parent,
                src_image,
                src_path: raw_src_path,
                dst_path: raw_dst_path,
            } => {
                let parent = translated_nodes[*parent].as_ref().unwrap();
                let src_image = translated_nodes[*src_image].as_ref().unwrap();
                let src_cwd = get_cwd_from_image_spec(&src_image.1);
                let src_path = src_cwd.join(raw_src_path);
                let dst_path = get_cwd_from_image_spec(&parent.1).join(raw_dst_path);
                let o = FileSystem::copy()
                    .from(LayerPath::Other(src_image.0.output(), src_path))
                    .to(OutputIdx(0), LayerPath::Other(parent.0.output(), dst_path))
                    .create_path(true)
                    .recursive(true)
                    .into_operation()
                    .custom_name(format!(
                        "...::copy({:?}, {:?})",
                        &raw_src_path, &raw_dst_path
                    ))
                    .ref_counted();
                (o.into(), parent.1.clone())
            }
            CopyFromLocal {
                parent,
                src_path,
                dst_path: raw_dst_path,
            } => {
                let parent = translated_nodes[*parent].as_ref().unwrap();
                let dst_path = get_cwd_from_image_spec(&parent.1).join(raw_dst_path);
                let o = FileSystem::copy()
                    .from(LayerPath::Other(local_context.clone(), src_path))
                    .to(OutputIdx(0), LayerPath::Other(parent.0.output(), dst_path))
                    .create_path(true)
                    .recursive(true)
                    .into_operation()
                    .custom_name(format!("copy({:?}, {:?})", &src_path, &raw_dst_path))
                    .ref_counted();
                (o.into(), parent.1.clone())
            }
            SetWorkdir {
                parent,
                new_workdir,
            } => {
                let parent = translated_nodes[*parent]
                    .as_ref()
                    .expect("Expected dependencies to already be built");
                let parent_config = &*parent.1;
                let parent_dir = get_cwd_from_image_spec(parent_config);
                let mut new_config = parent_config.clone();
                new_config
                    .config
                    .get_or_insert_with(empty_image_config)
                    .working_dir = Some(parent_dir.join(new_workdir));
                let new_config = Arc::new(new_config);
                (parent.0.clone(), new_config)
            }
            SetEntrypoint {
                parent,
                new_entrypoint,
            } => {
                let (p_out, p_conf) = translated_nodes[*parent].clone().unwrap();
                let mut p_conf = (*p_conf).clone();
                p_conf
                    .config
                    .get_or_insert_with(empty_image_config)
                    .entrypoint = Some(new_entrypoint.to_owned());
                (p_out, Arc::new(p_conf))
            }
            SetLabel {
                parent,
                label,
                value,
            } => {
                let (p_out, p_conf) = translated_nodes[*parent].clone().unwrap();
                let mut p_conf = (*p_conf).clone();
                p_conf
                    .config
                    .get_or_insert_with(empty_image_config)
                    .labels
                    .get_or_insert_with(BTreeMap::new)
                    .insert(label.to_owned(), value.to_owned());
                (p_out, Arc::new(p_conf))
            }
        };
        translated_nodes[node_id] = Some(new_node);
    }
    let mut outputs: Vec<(OwnedOutput, Arc<ImageSpecification>)> = Vec::new();
    for o in &build_plan.outputs {
        outputs.push(
            translated_nodes[o.node]
                .clone()
                .expect("Expected output to be built"),
        );
    }
    outputs
}
