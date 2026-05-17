use anyhow::Result;

use super::step::PlannedStep;

pub async fn execute_plan(plan: &[PlannedStep]) -> Result<()> {
    for step in plan {
        println!("pending: {}", step.title);
    }
    Ok(())
}
