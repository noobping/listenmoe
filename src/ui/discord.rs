type DiscordResult = Result<(), Box<dyn std::error::Error>>;

mod imp {
    use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

    use super::DiscordResult;

    const DISCORD_CLIENT_ID: &str = "1469290259853349040";

    pub struct Discord {
        enabled: bool,
        client: DiscordIpcClient,
        connected: bool,
    }

    impl Discord {
        pub fn new(enabled: bool) -> Self {
            let mut discord = Self {
                enabled,
                client: DiscordIpcClient::new(DISCORD_CLIENT_ID),
                connected: false,
            };
            if enabled {
                let _ = discord.connect();
            }
            discord
        }

        pub fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn connect(&mut self) -> DiscordResult {
            if self.connected {
                return Ok(());
            }
            self.client.connect()?;
            self.connected = true;
            Ok(())
        }

        fn reconnect(&mut self) -> DiscordResult {
            let _ = self.client.close();
            self.connected = false;
            self.client.connect()?;
            self.connected = true;
            Ok(())
        }

        fn with_reconnect_retry<F>(&mut self, mut f: F) -> DiscordResult
        where
            F: FnMut(&mut Self) -> DiscordResult,
        {
            if f(self).is_err() {
                self.reconnect()?;
                f(self)?;
            }
            Ok(())
        }

        fn set_once(&mut self, details: &str, state: &str) -> DiscordResult {
            let payload = activity::Activity::new().details(details).state(state);
            self.client.set_activity(payload)?;
            Ok(())
        }

        fn clear_once(&mut self) -> DiscordResult {
            self.client.clear_activity()?;
            Ok(())
        }

        pub fn set(&mut self, details: &str, state: &str) -> DiscordResult {
            if !self.enabled {
                return Ok(());
            }
            self.connect()?;
            self.with_reconnect_retry(|s| s.set_once(details, state))
        }

        pub fn clear(&mut self) -> DiscordResult {
            if !self.enabled {
                return Ok(());
            }
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
}

pub use imp::Discord;
