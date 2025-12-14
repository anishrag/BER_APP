use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct DataLoader {
    // Fields for DataLoader
}

impl DataLoader {
    pub fn new() -> Self {
        DataLoader {
            // Initialize fields
        }
    }

    pub fn load_general_data(
        &self,
        tx: Sender<(String, f32)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Logic to load general data from a fixed source
        self.simulate_loading_data(tx)?;
        Ok(())
    }

    pub fn simulate_loading_data(&self, tx: Sender<(String, f32)>) -> Result<(), String> {
        // Logic to simulate data loading
        let steps = vec![
            "Loading config",
            "Initializing modules",
            "Loading assets",
            "Finalizing",
        ];
        let total = steps.len();

        for (i, step) in steps.iter().enumerate() {
            thread::sleep(Duration::from_secs(1)); // Reduced for testing

            let progress = (i + 1) as f32 / total as f32;
            println!("Loader thread: {} - {:.1}%", step, progress * 100.0);

            if tx.send((step.to_string(), progress)).is_err() {
                println!("Channel closed, stopping loader thread");
                break;
            }
        }
        println!("Loader thread finished");

        Ok(())
    }
}
