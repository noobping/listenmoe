use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

const DISCORD_CLIENT_ID: &str = "1469290259853349040";

pub struct Discord {
    client: DiscordIpcClient,
}

impl Discord {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut client = DiscordIpcClient::new(DISCORD_CLIENT_ID);
        client.connect()?;
        Ok(Self { client })
    }

    pub fn set(&mut self, details: &str, state: &str) -> Result<(), Box<dyn std::error::Error>> {
        let payload = activity::Activity::new().details(details).state(state);
        self.client.set_activity(payload)?;
        Ok(())
    }

    pub fn clear(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.client.clear_activity()?;
        Ok(())
    }
}

impl Drop for Discord {
    fn drop(&mut self) {
        let _ = self.clear();
        let _ = self.client.close();
    }
}
