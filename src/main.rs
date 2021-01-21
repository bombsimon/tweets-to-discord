use egg_mode::{user::TwitterUser, KeyPair, Response, Token};
use env_logger::Env;
use futures::TryStreamExt;
use log::{error, info};
use serde::Deserialize;
use serenity::{
    async_trait,
    model::{gateway::Ready, id::ChannelId},
    prelude::*,
};

/// TwitterConfig represents all the configuration required for Twitter.
#[derive(Deserialize)]
struct TwitterConfig {
    follow: String,
    consumer_key: String,
    consumer_secret: String,
    access_token: String,
    access_token_secret: String,
}

/// DiscordConfig represents all the configuration required for Discord.
#[derive(Clone, Deserialize)]
struct DiscordConfig {
    token: String,
    channel_id: u64,
    header: String,
    time: String,
    text: String,
    reply: String,
    quote: String,
    url: String,
}

/// Config represents the full configuration file with all configuration.
#[derive(Deserialize)]
struct Config {
    twitter: TwitterConfig,
    discord: DiscordConfig,
}

/// TwitterService represents the service that knows about the user at Twitter to watch and the
/// token to use when calling the APIs. The user field is a `TwitterUser` and can be used to show
/// things such as display name and ID.
struct TwitterService {
    user: Response<TwitterUser>,
    token: Token,
}

impl TwitterService {
    /// Create a new TwitterService with the passed config. A token will be created with all the
    /// credentials and with this token the user to watch will be fetched. This means that the new
    /// constructor will fail if the credentials is wrong or if the user does not exist (or is
    /// private and not followed).
    async fn new(config: TwitterConfig) -> TwitterService {
        let con_token = KeyPair::new(config.consumer_key, config.consumer_secret);
        let access_token = KeyPair::new(config.access_token, config.access_token_secret);
        let token = Token::Access {
            consumer: con_token,
            access: access_token,
        };

        let user = egg_mode::user::show(config.follow, &token).await.unwrap();

        TwitterService { user, token }
    }

    /// Stream the feed with everything coming from the watched user. The context and channel ID
    /// passed comes from the Discord ready handler so this can be used when sending tweets to
    /// Discord.
    async fn stream(&self, ctx: Context, config: &DiscordConfig) {
        let stream = egg_mode::stream::filter()
            .follow(&[self.user.id])
            .start(&self.token);

        info!("Starting stream, watching {}", self.user.name);

        stream
            .try_for_each(|m| {
                if let egg_mode::stream::StreamMessage::Tweet(tweet) = m {
                    // TODO: Figure out how to handle the move of these variables without creating
                    // a copy.
                    let t = tweet.clone();
                    let c = ctx.clone();
                    let config = config.clone();
                    let tkn = self.token.clone();

                    // We only care about tweets sent from the actual user, not any mention from
                    // anyone.
                    if tweet.user.unwrap().id != self.user.id {
                        return futures::future::ok(());
                    }

                    info!("@{}: {}", self.user.screen_name, t.text);

                    let tweet_url = format!(
                        "https://twitter.com/{}/status/{}",
                        self.user.screen_name, t.id
                    );

                    // Since the closure isn't async we spawn a green thread with tokio to handle
                    // the asyn call to `send_message`. This will send a message to the configured
                    // Discord channel as a block message. If it fails to send, the error will be
                    // printed to the screen.
                    tokio::spawn(async move {
                        let reply = match t.in_reply_to_status_id {
                            Some(reply_id) => {
                                let reply_tweet =
                                    egg_mode::tweet::show(reply_id, &tkn).await.unwrap();
                                Some(reply_tweet.text.clone())
                            }
                            None => None,
                        };

                        if let Err(why) = ChannelId(config.channel_id)
                            .send_message(&c, |m| {
                                m.embed(|e| {
                                    e.title(config.header);
                                    e.field(
                                        config.time,
                                        t.created_at
                                            .with_timezone(&chrono::Local)
                                            .format("%Y-%m-%d %H:%M:%S"),
                                        false,
                                    );
                                    e.field(config.text, t.text, false);

                                    if let Some(r) = reply {
                                        e.field(config.reply, r, false);
                                    }

                                    if let Some(q) = t.quoted_status {
                                        e.field(config.quote, q.text, false);
                                    }

                                    e.field(config.url, tweet_url, false);

                                    e
                                })
                            })
                            .await
                        {
                            error!("Error sending message: {:?}", why);
                        };
                    });
                }

                futures::future::ok(())
            })
            .await
            .expect("Stream error");
    }
}

/// Handler is the Discord handler that is used to implement the EventHandler.
struct Handler {
    twitter_service: TwitterService,
    config: DiscordConfig,
}

#[async_trait]
impl EventHandler for Handler {
    /// We only implement ready since it's called whenever we successfully start the Discord
    /// client. As soon as we're ready we start the twitter stream with the channel ID found on our
    /// handler as channel destination.
    async fn ready(&self, ctx: Context, _ready: Ready) {
        self.twitter_service.stream(ctx, &self.config).await;
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("tweets_to_discord")).init();

    let args: Vec<String> = std::env::args().collect();
    let config_file = match args.len() {
        1 => "config.yaml",
        _ => args[1].as_str(),
    };

    let f = std::fs::File::open(config_file).unwrap();
    let config: Config = serde_yaml::from_reader(f).unwrap();

    let twitter_service = TwitterService::new(config.twitter);
    let mut client = serenity::client::Client::builder(config.discord.token.as_str())
        .event_handler(Handler {
            twitter_service: twitter_service.await,
            config: config.discord,
        })
        .await
        .expect("Error creating client");

    if let Err(why) = client.start().await {
        error!("Client error: {:?}", why);
    }
}
