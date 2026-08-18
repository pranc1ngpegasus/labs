use std::{fs, process::Command};

#[cfg(unix)]
#[test]
fn top_level_init_installs_both_skills_for_all_agents_and_scopes()
-> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_ren"))
        .args(["init", "--project"])
        .current_dir(project.path())
        .output()?;
    assert!(
        output.status.success(),
        "ren init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let user = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_ren"))
        .args(["init", "--user"])
        .current_dir(project.path())
        .env("HOME", user.path())
        .output()?;
    assert!(
        output.status.success(),
        "ren init --user failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    for config_dir in [".claude", ".cursor", ".codex", ".grok"] {
        for base in [project.path(), user.path()] {
            let skill_root = base.join(config_dir).join("skills");
            assert_eq!(
                fs::read_to_string(skill_root.join("ren-workflow/SKILL.md"))?,
                ren_workflow::SKILL_MD
            );
            assert_eq!(
                fs::read_to_string(skill_root.join("ren-workflow/agents/openai.yaml"))?,
                ren_workflow::OPENAI_YAML
            );
            assert_eq!(
                fs::read_to_string(skill_root.join("ren-memory/SKILL.md"))?,
                ren_memory::MEMORY_SKILL_MD
            );
            assert_eq!(
                fs::read_to_string(skill_root.join("ren-memory/agents/openai.yaml"))?,
                ren_memory::MEMORY_OPENAI_YAML
            );
        }
    }

    // pi keeps user-global skills under `.pi/agent/skills` but project skills
    // under `.pi/skills`.
    for skill_root in [
        project.path().join(".pi/skills"),
        user.path().join(".pi/agent/skills"),
    ] {
        assert_eq!(
            fs::read_to_string(skill_root.join("ren-workflow/SKILL.md"))?,
            ren_workflow::SKILL_MD
        );
        assert_eq!(
            fs::read_to_string(skill_root.join("ren-workflow/agents/openai.yaml"))?,
            ren_workflow::OPENAI_YAML
        );
        assert_eq!(
            fs::read_to_string(skill_root.join("ren-memory/SKILL.md"))?,
            ren_memory::MEMORY_SKILL_MD
        );
        assert_eq!(
            fs::read_to_string(skill_root.join("ren-memory/agents/openai.yaml"))?,
            ren_memory::MEMORY_OPENAI_YAML
        );
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn top_level_init_adds_memory_to_an_existing_workflow_install()
-> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let skill_root = project.path().join(".codex/skills");

    let output = Command::new(env!("CARGO_BIN_EXE_ren"))
        .args(["workflow", "init", "--project", "--agent", "codex"])
        .current_dir(project.path())
        .output()?;
    assert!(
        output.status.success(),
        "ren workflow init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(skill_root.join("ren-workflow/SKILL.md").is_file());
    assert!(skill_root.join("ren-workflow/agents/openai.yaml").is_file());
    assert!(!skill_root.join("ren-memory").exists());

    let output = Command::new(env!("CARGO_BIN_EXE_ren"))
        .args(["init", "--project", "--agent", "codex"])
        .current_dir(project.path())
        .output()?;
    assert!(
        output.status.success(),
        "ren init failed to add memory skill: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(skill_root.join("ren-workflow/SKILL.md"))?,
        ren_workflow::SKILL_MD
    );
    assert_eq!(
        fs::read_to_string(skill_root.join("ren-memory/SKILL.md"))?,
        ren_memory::MEMORY_SKILL_MD
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn top_level_init_preflights_all_skills_before_writing() -> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let skill_root = project.path().join(".codex/skills");
    let metadata = skill_root.join("ren-memory/agents/openai.yaml");
    fs::create_dir_all(
        metadata
            .parent()
            .ok_or("metadata path must have a parent")?,
    )?;
    fs::write(&metadata, "user-owned metadata")?;

    let output = Command::new(env!("CARGO_BIN_EXE_ren"))
        .args(["init", "--project", "--agent", "codex"])
        .current_dir(project.path())
        .output()?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("skill file already exists"),
        "unexpected ren init error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!skill_root.join("ren-workflow/SKILL.md").exists());
    assert!(!skill_root.join("ren-workflow/agents/openai.yaml").exists());
    assert!(!skill_root.join("ren-memory/SKILL.md").exists());
    assert_eq!(fs::read_to_string(metadata)?, "user-owned metadata");
    Ok(())
}

#[test]
fn memory_init_only_initializes_memory_home_and_vault() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let memory_home = temporary.path().join("memory");
    let project = temporary.path().join("project");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&project)?;

    let output = Command::new(env!("CARGO_BIN_EXE_ren"))
        .args([
            "memory",
            "init",
            "--user",
            "--project",
            project.to_string_lossy().as_ref(),
        ])
        .current_dir(&project)
        .env("HOME", &home)
        .env("REN_MEMORY_HOME", &memory_home)
        .output()?;
    assert!(
        output.status.success(),
        "ren memory init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(memory_home.join("registry.json").is_file());
    assert!(memory_home.join("config.toml").is_file());
    assert!(!home.join(".codex/skills/ren-memory").exists());
    assert!(!project.join(".codex/skills/ren-memory").exists());
    Ok(())
}
