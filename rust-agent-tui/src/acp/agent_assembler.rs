use std::sync::Arc;

use rust_agent_middlewares::agent_define::AgentOverrides;
use rust_agent_middlewares::prelude::*;
use rust_agent_middlewares::tools::TodoItem;
use rust_create_agent::agent::events::AgentEventHandler;
use rust_create_agent::agent::state::AgentState;
use rust_create_agent::agent::{AgentCancellationToken, ReActAgent};
use rust_create_agent::interaction::UserInteractionBroker;
use rust_create_agent::llm::{BaseModelReactLLM, RetryConfig, RetryableLLM};

use crate::app::agent::LlmProvider;
use crate::config::PeriConfig;

pub type PeriLlm = RetryableLLM<BaseModelReactLLM>;
pub type PeriReActAgent = ReActAgent<PeriLlm, AgentState>;

pub struct AgentAssembleConfig {
    pub provider: LlmProvider,
    pub cwd: String,
    pub system_prompt: String,
    pub broker: Arc<dyn UserInteractionBroker>,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub peri_config: Arc<PeriConfig>,
    pub preload_skills: Vec<String>,
    pub event_handler: Arc<dyn AgentEventHandler>,
    pub cancel: AgentCancellationToken,
    pub cron_scheduler:
        Option<Arc<parking_lot::Mutex<rust_agent_middlewares::cron::CronScheduler>>>,
    /// Agent overrides from CLI --agent (persona, tone, proactiveness)
    pub agent_overrides: Option<AgentOverrides>,
}

pub fn assemble_agent(
    config: AgentAssembleConfig,
) -> (PeriReActAgent, tokio::sync::mpsc::Receiver<Vec<TodoItem>>) {
    let AgentAssembleConfig {
        provider,
        cwd,
        system_prompt,
        broker,
        permission_mode,
        peri_config,
        preload_skills,
        event_handler,
        cancel,
        cron_scheduler,
        agent_overrides,
    } = config;

    // Apply agent overrides to system prompt
    let system_prompt = agent_overrides.as_ref().map_or_else(
        || system_prompt.clone(),
        |ov| {
            crate::prompt::build_system_prompt(
                Some(ov),
                &cwd,
                crate::prompt::PromptFeatures::detect(),
                &[],
            )
        },
    );

    let provider_for_factory = provider.clone();

    // LLM
    let model = RetryableLLM::new(
        BaseModelReactLLM::new(provider.into_model()),
        RetryConfig::default(),
    )
    .with_event_handler(Arc::clone(&event_handler));

    // Todo channel
    let (todo_tx, todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);

    // HITL middleware（Auto 模式需要 LLM 分类器）
    let auto_classifier: Option<Arc<dyn AutoClassifier>> =
        Some(Arc::new(LlmAutoClassifier::new(Arc::new(
            tokio::sync::Mutex::new(provider_for_factory.clone().into_model()),
        ))));
    let hitl = HumanInTheLoopMiddleware::with_shared_mode(
        broker.clone(),
        default_requires_approval,
        permission_mode,
        auto_classifier,
    );

    // AskUser 工具
    let ask_user_tool = AskUserTool::new(broker);

    // 父工具集（供子 agent 继承）
    let mut parent_tools: Vec<Box<dyn rust_create_agent::tools::BaseTool>> =
        FilesystemMiddleware::build_tools(&cwd);
    parent_tools.extend(TerminalMiddleware::build_tools(&cwd));

    // 子 agent LLM 工厂
    let provider_clone = provider_for_factory;
    let config_for_factory = peri_config;
    #[allow(clippy::type_complexity)]
    let llm_factory: Arc<
        dyn Fn(Option<&str>) -> Box<dyn rust_create_agent::agent::react::ReactLLM + Send + Sync>
            + Send
            + Sync,
    > = Arc::new(move |model_alias: Option<&str>| {
        if let Some(alias) = model_alias {
            if let Some(p) = LlmProvider::from_config_for_alias(&config_for_factory, alias) {
                return Box::new(RetryableLLM::new(
                    BaseModelReactLLM::new(p.into_model()),
                    RetryConfig::default(),
                ));
            }
        }
        Box::new(RetryableLLM::new(
            BaseModelReactLLM::new(provider_clone.clone().into_model()),
            RetryConfig::default(),
        ))
    });

    // 系统提示词构建器
    #[allow(clippy::type_complexity)]
    let system_builder: Arc<
        dyn Fn(Option<&rust_agent_middlewares::AgentOverrides>, &str) -> String + Send + Sync,
    > = Arc::new(|overrides, cwd_dir| {
        crate::prompt::build_system_prompt(
            overrides,
            cwd_dir,
            crate::prompt::PromptFeatures::detect(),
            &[],
        )
    });

    // SubAgent 中间件
    let subagent = SubAgentMiddleware::new(
        parent_tools,
        Some(Arc::clone(&event_handler) as Arc<dyn AgentEventHandler>),
        llm_factory,
    )
    .with_system_builder(system_builder)
    .with_cancel(cancel);

    // 构建 ReActAgent
    let executor = ReActAgent::new(model)
        .max_iterations(500)
        .with_system_prompt(system_prompt)
        .add_middleware(Box::new(AgentsMdMiddleware::new()))
        .add_middleware(Box::new(AgentDefineMiddleware::new()))
        .add_middleware(Box::new(SkillsMiddleware::new()))
        .add_middleware(Box::new(SkillPreloadMiddleware::new(preload_skills, &cwd)))
        .add_middleware(Box::new(FilesystemMiddleware::new()))
        .add_middleware(Box::new(TerminalMiddleware::new()))
        .add_middleware(Box::new(TodoMiddleware::new(todo_tx)))
        .add_middleware(Box::new(rust_agent_middlewares::cron::CronMiddleware::new(
            cron_scheduler.unwrap_or_else(|| {
                Arc::new(parking_lot::Mutex::new(
                    rust_agent_middlewares::cron::CronScheduler::new(
                        tokio::sync::mpsc::unbounded_channel().0,
                    ),
                ))
            }),
        )))
        .add_middleware(Box::new(hitl))
        .add_middleware(Box::new(subagent))
        .with_event_handler(Arc::clone(&event_handler))
        .register_tool(Box::new(ask_user_tool));

    (executor, todo_rx)
}
