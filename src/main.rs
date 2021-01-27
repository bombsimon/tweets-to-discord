use egg_mode::{
    tweet::{DraftTweet, Tweet},
    user::TwitterUser,
    KeyPair, Response, Token,
};
use env_logger::Env;
use futures::TryStreamExt;
use log::{error, info, trace};
use serde::Deserialize;
use serenity::{
    async_trait,
    builder::CreateMessage,
    model::{channel::Embed, gateway::Ready, id::ChannelId, prelude::Message},
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
    tweet_replies: bool,
    tweet_as_user: bool,
    embed: DiscordConfigEmbed,
    plaintext: DiscordConfigPlaintext,
}

#[derive(Clone, Deserialize)]
struct DiscordConfigEmbed {
    header: String,
    text: String,
    reply: String,
    quote: String,
    url: String,
}

#[derive(Clone, Deserialize)]
struct DiscordConfigPlaintext {
    reply_prefix: String,
    reply_postfix: String,
    quote_prefix: String,
    quote_postfix: String,
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
        let mut stream = egg_mode::stream::filter()
            .follow(&[self.user.id])
            .start(&self.token);

        info!("starting stream, watching {}", self.user.name);

        while let Ok(m) = stream.try_next().await {
            if let Some(egg_mode::stream::StreamMessage::Tweet(tweet)) = m {
                trace!("tweet received in stream");
                self.handle_message(&ctx, &config, tweet).await;
            }
        }
    }

    /// Handle the message that got received in the Twitter stream. If the tweet follows required
    /// criterias, an embedded message will be constructed and posted to those channels configured.
    async fn handle_message(&self, ctx: &Context, config: &DiscordConfig, tweet: Tweet) {
        let tweeting_user = tweet.user.as_ref().unwrap();

        if tweeting_user.id != self.user.id {
            trace!(
                "Tweet matched filter but was from {}, not {}. Will not post",
                tweeting_user.screen_name,
                self.user.screen_name
            );
            return;
        }

        let tweet_url = format!(
            "https://twitter.com/{}/status/{}",
            self.user.screen_name, tweet.id
        );

        trace!("@{}: {} ({})", self.user.screen_name, tweet.text, tweet_url);

        // Since the embed closure isn't async we fetch the tweet replied to if this is a reply.
        let reply = match tweet.in_reply_to_status_id {
            Some(reply_id) => {
                let reply_tweet = egg_mode::tweet::show(reply_id, &self.token).await.unwrap();
                Some(reply_tweet.text.clone())
            }
            None => None,
        };

        let result = ChannelId(config.channel_id)
            .send_message(ctx, |m| {
                if config.tweet_as_user {
                    self.create_plaintext_message(m, &config, tweet, reply);
                } else {
                    self.create_embeded_message(m, &config, tweet, tweet_url, reply);
                }

                m
            })
            .await;

        if let Err(why) = result {
            error!("error sending message: {:?}", why);
        } else {
            trace!("sent message to {} successfully", config.channel_id);
        };
    }

    /// Create a plaintext message to write from the bot just like if it would've written what's
    /// happened on Twitter straight to the chat. This will make it appear a bit more like it's a
    /// human writing the post.
    fn create_plaintext_message(
        &self,
        m: &mut CreateMessage,
        config: &DiscordConfig,
        tweet: Tweet,
        reply: Option<String>,
    ) {
        let mut content = "".to_string();
        if let Some(r) = reply {
            content.push_str(format!("{}\n", &config.plaintext.reply_prefix).as_str());
            content.push_str(format!("> {}\n", r).as_str());
            content.push_str(format!("{}\n", &config.plaintext.reply_postfix).as_str());
        }

        if let Some(q) = tweet.quoted_status {
            content.push_str(format!("{}\n", &config.plaintext.quote_prefix).as_str());
            content.push_str(format!("> {}\n", q.text).as_str());
            content.push_str(format!("{}\n", &config.plaintext.quote_postfix).as_str());
        }

        content.push_str(tweet.text.as_str());

        m.content(content);
    }

    /// Create an embedded message to post to Discord. This message will hold headers and content
    /// and it will be obvious that it's a bot that posted it.
    fn create_embeded_message(
        &self,
        m: &mut CreateMessage,
        config: &DiscordConfig,
        tweet: Tweet,
        tweet_url: String,
        reply: Option<String>,
    ) {
        m.embed(|e| {
            e.title(&config.embed.header);
            e.field(&config.embed.text, tweet.text, false);

            if let Some(r) = reply {
                e.field(&config.embed.reply, r, false);
            }

            if let Some(q) = tweet.quoted_status {
                e.field(&config.embed.quote, q.text, false);
            }

            e.field(&config.embed.url, tweet_url, false);

            e
        });
    }
}

/// Handler is the Discord handler that is used to implement the EventHandler.
struct Handler {
    twitter_service: TwitterService,
    config: DiscordConfig,
}

impl Handler {
    /// Check all embedded contents and for each one check every field. If the field is named
    /// what's configured as the URL, try to extract the Tweet ID (last part) and return it as u64.
    fn tweet_id_from_embeds(&self, embeds: &[Embed]) -> Result<u64, std::io::ErrorKind> {
        for embed in embeds {
            for field in &embed.fields {
                if field.name == self.config.embed.url {
                    return Ok(std::path::Path::new(&field.value)
                        .file_name()
                        .ok_or(std::io::ErrorKind::InvalidInput)?
                        .to_str()
                        .ok_or(std::io::ErrorKind::InvalidInput)?
                        .parse::<u64>()
                        .or(Err(std::io::ErrorKind::InvalidData)))?;
                }
            }
        }

        Err(std::io::ErrorKind::NotFound)
    }

    /// Parse a Discord message containing a tweet. If it's a reply to the bot itself, check if
    /// it's a message with embedded data and if it's possible to extract a tweet ID. If this is
    /// possible, send the reply to the tweet.
    async fn reply_to_tweet(&self, reply: &Box<Message>, ctx: &Context, msg: &Message) {
        if reply.author.id != ctx.cache.current_user_id().await {
            return;
        }

        if let Ok(tweet_id) = self.tweet_id_from_embeds(&reply.embeds) {
            let draft = DraftTweet::new(format!(
                "@{} {}",
                self.twitter_service.user.screen_name, msg.content
            ))
            .in_reply_to(tweet_id);

            let tweet = draft.send(&self.twitter_service.token).await;

            match tweet {
                Ok(t) => {
                    let tweet_url = format!(
                        "https://twitter.com/{}/status/{}",
                        t.user.as_ref().unwrap().screen_name,
                        t.id
                    );

                    let _ = msg
                        .channel_id
                        .say(&ctx, format!("Postet reply: {}", tweet_url))
                        .await;
                }
                Err(why) => error!("failed to post tweet: {}", why),
            }
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    /// We only implement ready since it's called whenever we successfully start the Discord
    /// client. As soon as we're ready we start the twitter stream with the channel ID found on our
    /// handler as channel destination.
    async fn ready(&self, ctx: Context, _ready: Ready) {
        self.twitter_service.stream(ctx, &self.config).await;
    }

    /// Check the message and see if it's a reply to ourself.
    async fn message(&self, ctx: Context, msg: Message) {
        if self.config.tweet_replies {
            if let Some(reply) = &msg.referenced_message {
                self.reply_to_tweet(&reply, &ctx, &msg).await;
            }
        }
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
        .expect("error creating client");

    if let Err(why) = client.start().await {
        error!("client error: {:?}", why);
    }
}
