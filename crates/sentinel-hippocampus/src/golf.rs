//! GOLF Framework — Goal-Oriented Life Tasks fuer Langzeit-Agent-Tracking.
//!
//! Agents gleichen Tages-Aktionen gegen Langzeitziele ab (Befoerderung,
//! Projektabschluss). Goals werden in einer dedizierten redb-Tabelle
//! persistiert und ueberleben Schichtwechsel und Daemon-Neustarts.
//!
//! TOGAF Reference: Memory Tier "Goal Memory" (GOLF Framework \[25\]).

use std::fmt;

/// Typ eines Langzeit-Ziels.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GoalType {
    /// Befoerderung, Rollenwechsel, Fuehrungsverantwortung
    Career,
    /// Projekt abschliessen, Feature liefern, Deadline einhalten
    Project,
    /// Beziehung aufbauen, Team integrieren, Netzwerk erweitern
    Social,
    /// Neue Faehigkeit lernen, Zertifizierung, Tooling
    Skill,
}

impl fmt::Display for GoalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GoalType::Career => write!(f, "career"),
            GoalType::Project => write!(f, "project"),
            GoalType::Social => write!(f, "social"),
            GoalType::Skill => write!(f, "skill"),
        }
    }
}

/// Lebenszyklusstatus eines Goals.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GoalStatus {
    /// Aktiv verfolgt
    Active,
    /// Erfolgreich abgeschlossen
    Completed,
    /// Gescheitert (Deadline ueberschritten, Bedingungen nicht erfuellt)
    Failed,
    /// Vom Agent aufgegeben
    Abandoned,
}

impl fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GoalStatus::Active => write!(f, "active"),
            GoalStatus::Completed => write!(f, "completed"),
            GoalStatus::Failed => write!(f, "failed"),
            GoalStatus::Abandoned => write!(f, "abandoned"),
        }
    }
}

/// Ein Langzeit-Ziel eines Agents (GOLF Framework).
///
/// Goals werden bei Agent-Spawn basierend auf der Rolle erstellt und
/// ueber die Simulationszeit aktualisiert. Progress wird als f64 [0.0, 1.0]
/// getrackt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    pub id: u64,
    /// Agent dem das Goal gehoert (z.B. "Thomas", "Lisa")
    pub agent_name: String,
    /// Kategorie des Goals
    pub goal_type: GoalType,
    /// Menschenlesbare Beschreibung
    pub description: String,
    /// Fortschritt [0.0, 1.0]
    pub progress: f64,
    /// Aktueller Status
    pub status: GoalStatus,
    /// Simulations-Tick bei Erstellung
    pub created_tick: u64,
    /// Optionale Deadline (Simulations-Tick)
    pub deadline_tick: Option<u64>,
    /// Letztes Update (Simulations-Tick)
    pub last_updated_tick: u64,
}

impl Goal {
    /// Erstellt ein neues aktives Goal.
    pub fn new(
        id: u64,
        agent_name: &str,
        goal_type: GoalType,
        description: &str,
        created_tick: u64,
        deadline_tick: Option<u64>,
    ) -> Self {
        Self {
            id,
            agent_name: agent_name.to_string(),
            goal_type,
            description: description.to_string(),
            progress: 0.0,
            status: GoalStatus::Active,
            created_tick,
            deadline_tick,
            last_updated_tick: created_tick,
        }
    }

    /// Aktualisiert den Fortschritt (clamped auf [0.0, 1.0]).
    ///
    /// Setzt automatisch `Completed` bei progress >= 1.0.
    pub fn update_progress(&mut self, progress: f64, tick: u64) {
        self.progress = progress.clamp(0.0, 1.0);
        self.last_updated_tick = tick;
        if self.progress >= 1.0 && self.status == GoalStatus::Active {
            self.status = GoalStatus::Completed;
        }
    }

    /// Ob das Goal noch aktiv ist.
    pub fn is_active(&self) -> bool {
        self.status == GoalStatus::Active
    }
}

/// Erzeugt Default-Goals basierend auf der Agent-Rolle.
///
/// Mapping:
/// - CEO/Manager/Teamlead → Career + Project
/// - Developer/Backend/Frontend → Project + Skill
/// - Designer/UX → Skill + Project
/// - HR/Psychologe/Arzt/Betriebsrat → Social + Career
/// - Sonstige → Project
pub fn default_goals_for_role(agent_name: &str, role: &str, tick: u64) -> Vec<Goal> {
    let role_lower = role.to_lowercase();
    let mut goals = Vec::new();
    let mut next_id = 1u64;

    // HR/Psychologe/Arzt/Betriebsrat MUSS vor Manager stehen,
    // weil "HR Managerin" sonst auf "manager" matched.
    if role_lower.contains("hr")
        || role_lower.contains("psycholog")
        || role_lower.contains("arzt")
        || role_lower.contains("betriebsrat")
    {
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Social,
            "Teamzusammenhalt und Wohlbefinden foerdern",
            tick,
            None,
        ));
        next_id += 1;
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Career,
            "Beratungskompetenz erweitern",
            tick,
            None,
        ));
    } else if role_lower.contains("ceo")
        || role_lower.contains("manager")
        || role_lower.contains("teamlead")
        || role_lower.contains("leitung")
    {
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Career,
            "Abteilung erfolgreich fuehren und Teamzufriedenheit steigern",
            tick,
            None,
        ));
        next_id += 1;
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Project,
            "Quartalsziele der Abteilung erreichen",
            tick,
            None,
        ));
    } else if role_lower.contains("develop")
        || role_lower.contains("backend")
        || role_lower.contains("frontend")
        || role_lower.contains("fullstack")
    {
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Project,
            "Aktuelles Projekt termingerecht abschliessen",
            tick,
            None,
        ));
        next_id += 1;
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Skill,
            "Neue Technologie oder Framework erlernen",
            tick,
            None,
        ));
    } else if role_lower.contains("design")
        || role_lower.contains("ux")
        || role_lower.contains("grafik")
    {
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Skill,
            "Neues Design-Tool oder Methodik erlernen",
            tick,
            None,
        ));
        next_id += 1;
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Project,
            "Design-System weiterentwickeln",
            tick,
            None,
        ));
    } else {
        goals.push(Goal::new(
            next_id,
            agent_name,
            GoalType::Project,
            "Arbeitsaufgaben zuverlaessig erledigen",
            tick,
            None,
        ));
    }

    let _ = next_id;
    goals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_goal_new() {
        let goal = Goal::new(
            1,
            "Thomas",
            GoalType::Career,
            "Befoerderung",
            100,
            Some(5000),
        );
        assert_eq!(goal.id, 1);
        assert_eq!(goal.agent_name, "Thomas");
        assert_eq!(goal.goal_type, GoalType::Career);
        assert_eq!(goal.description, "Befoerderung");
        assert_eq!(goal.progress, 0.0);
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.created_tick, 100);
        assert_eq!(goal.deadline_tick, Some(5000));
        assert_eq!(goal.last_updated_tick, 100);
        assert!(goal.is_active());
    }

    #[test]
    fn test_goal_progress_update() {
        let mut goal = Goal::new(1, "Thomas", GoalType::Project, "Feature", 0, None);
        goal.update_progress(0.5, 100);
        assert_eq!(goal.progress, 0.5);
        assert_eq!(goal.last_updated_tick, 100);
        assert!(goal.is_active());
    }

    #[test]
    fn test_goal_auto_complete_at_100() {
        let mut goal = Goal::new(1, "Thomas", GoalType::Skill, "Rust lernen", 0, None);
        goal.update_progress(1.0, 500);
        assert_eq!(goal.progress, 1.0);
        assert_eq!(goal.status, GoalStatus::Completed);
        assert!(!goal.is_active());
    }

    #[test]
    fn test_goal_progress_clamped() {
        let mut goal = Goal::new(1, "Thomas", GoalType::Career, "Test", 0, None);
        goal.update_progress(1.5, 10);
        assert_eq!(goal.progress, 1.0);

        let mut goal2 = Goal::new(2, "Lisa", GoalType::Social, "Test", 0, None);
        goal2.update_progress(-0.5, 10);
        assert_eq!(goal2.progress, 0.0);
    }

    #[test]
    fn test_goal_completed_stays_completed() {
        let mut goal = Goal::new(1, "Thomas", GoalType::Project, "Test", 0, None);
        goal.update_progress(1.0, 100);
        assert_eq!(goal.status, GoalStatus::Completed);

        // Progress update on completed goal does not revert status
        goal.update_progress(0.5, 200);
        assert_eq!(goal.progress, 0.5);
        // Status stays Completed (update_progress only sets Completed, never reverts)
        assert_eq!(goal.status, GoalStatus::Completed);
    }

    #[test]
    fn test_goal_type_display() {
        assert_eq!(GoalType::Career.to_string(), "career");
        assert_eq!(GoalType::Project.to_string(), "project");
        assert_eq!(GoalType::Social.to_string(), "social");
        assert_eq!(GoalType::Skill.to_string(), "skill");
    }

    #[test]
    fn test_goal_status_display() {
        assert_eq!(GoalStatus::Active.to_string(), "active");
        assert_eq!(GoalStatus::Completed.to_string(), "completed");
        assert_eq!(GoalStatus::Failed.to_string(), "failed");
        assert_eq!(GoalStatus::Abandoned.to_string(), "abandoned");
    }

    #[test]
    fn test_default_goals_ceo() {
        let goals = default_goals_for_role("Thomas", "CEO", 0);
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0].goal_type, GoalType::Career);
        assert_eq!(goals[1].goal_type, GoalType::Project);
        assert!(goals[0].description.contains("Abteilung"));
    }

    #[test]
    fn test_default_goals_developer() {
        let goals = default_goals_for_role("Andreas", "Senior Developer", 100);
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0].goal_type, GoalType::Project);
        assert_eq!(goals[1].goal_type, GoalType::Skill);
    }

    #[test]
    fn test_default_goals_designer() {
        let goals = default_goals_for_role("Lisa", "UX Designer", 50);
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0].goal_type, GoalType::Skill);
        assert_eq!(goals[1].goal_type, GoalType::Project);
    }

    #[test]
    fn test_default_goals_hr() {
        let goals = default_goals_for_role("Monika", "HR Managerin", 0);
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0].goal_type, GoalType::Social);
        assert_eq!(goals[1].goal_type, GoalType::Career);
    }

    #[test]
    fn test_default_goals_unknown_role() {
        let goals = default_goals_for_role("Max", "Praktikant", 0);
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].goal_type, GoalType::Project);
    }

    #[test]
    fn test_goal_serialization_roundtrip() {
        let goal = Goal::new(
            42,
            "Thomas",
            GoalType::Career,
            "Befoerderung zum CTO",
            1000,
            Some(50000),
        );
        let json = serde_json::to_string(&goal).unwrap();
        let deserialized: Goal = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, 42);
        assert_eq!(deserialized.agent_name, "Thomas");
        assert_eq!(deserialized.goal_type, GoalType::Career);
        assert_eq!(deserialized.description, "Befoerderung zum CTO");
        assert_eq!(deserialized.deadline_tick, Some(50000));
    }
}
