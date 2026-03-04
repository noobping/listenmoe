use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

const DISCORD_CLIENT_ID: &str = "1469290259853349040";

pub struct Discord {
    client: DiscordIpcClient,
    connected: bool,
}

impl Discord {
    pub fn new() -> Self {
        let mut discord = Self {
            client: DiscordIpcClient::new(DISCORD_CLIENT_ID),
            connected: false,
        };
        let _ = discord.connect();
        discord
    }

    fn connect(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.connected {
            return Ok(());
        }
        self.client.connect()?;
        self.connected = true;
        Ok(())
    }

    fn reconnect(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let _ = self.client.close();
        self.connected = false;
        self.client.connect()?;
        self.connected = true;
        Ok(())
    }

    fn with_reconnect_retry<F>(&mut self, mut f: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: FnMut(&mut Self) -> Result<(), Box<dyn std::error::Error>>,
    {
        if f(self).is_err() {
            self.reconnect()?;
            f(self)?;
        }
        Ok(())
    }

    fn set_once(&mut self, details: &str, state: &str) -> Result<(), Box<dyn std::error::Error>> {
        let payload = activity::Activity::new().details(details).state(state);
        self.client.set_activity(payload)?;
        Ok(())
    }

    fn clear_once(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.client.clear_activity()?;
        Ok(())
    }

    pub fn set(&mut self, details: &str, state: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.connect()?;
        self.with_reconnect_retry(|s| s.set_once(details, state))
    }

    pub fn clear(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.connect()?;
        self.with_reconnect_retry(|s| s.clear_once())
    }
}

impl Drop for Discord {
    fn drop(&mut self) {
        if self.connected {
            let _ = self.client.clear_activity();
            let _ = self.client.close();
            self.connected = false;
        }
    }
}
