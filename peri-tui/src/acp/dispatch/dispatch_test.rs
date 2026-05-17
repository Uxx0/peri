    #[test]
    fn test_new_session_response_serialization() {
        let modes = SessionModeState::new(
            SessionModeId::new("auto"),
            vec![
                SessionMode::new(SessionModeId::new("auto"), "Auto"),
                SessionMode::new(SessionModeId::new("default"), "Default"),
            ],
        );

        let resp = NewSessionResponse::new("test-session-1")
            .modes(Some(modes))
            .config_options(Some(vec![
                agent_client_protocol::schema::SessionConfigOption::new(
                    SessionConfigId::new("thinking_effort"),
                    "Thinking Effort",
                    SessionConfigKind::Select(SessionConfigSelect::new(
                        SessionConfigValueId::new("high"),
                        vec![
                            SessionConfigSelectOption::new(SessionConfigValueId::new("low"), "Low"),
                            SessionConfigSelectOption::new(
                                SessionConfigValueId::new("high"),
                                "High",
                            ),
                        ],
                    )),
                ),
            ]));

        let json = serde_json::to_string_pretty(&resp).unwrap();
        eprintln!("NewSessionResponse JSON:\n{}", json);

        assert!(
            json.contains("\"modes\""),
            "modes field should be present in JSON"
        );
        assert!(
            json.contains("\"currentModeId\""),
            "currentModeId should be present"
        );
        assert!(
            json.contains("\"availableModes\""),
            "availableModes should be present"
        );
        assert!(
            json.contains("\"configOptions\""),
            "configOptions should be present"
        );
    }

    #[test]
    fn test_session_model_state_serialization() {
        let state = SessionModelState::new(
            ModelId::new("sonnet"),
            vec![
                ModelInfo::new(ModelId::new("opus"), "Claude Opus"),
                ModelInfo::new(ModelId::new("sonnet"), "Claude Sonnet"),
            ],
        );

        let resp = NewSessionResponse::new("test-session-1").models(Some(state));

        let json = serde_json::to_string_pretty(&resp).unwrap();
        eprintln!("NewSessionResponse with models JSON:\n{}", json);

        assert!(
            json.contains("\"models\""),
            "models field should be present in JSON"
        );
        assert!(
            json.contains("\"currentModelId\""),
            "currentModelId should be present"
        );
    }
