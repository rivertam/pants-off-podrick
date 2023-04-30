use anyhow::anyhow;
use chrono::{Datelike, Timelike};
use serenity::async_trait;
use serenity::futures::StreamExt;
use serenity::model::application::interaction::{Interaction, InteractionResponseType};
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::prelude::*;
use serenity::prelude::*;
use shuttle_secrets::SecretStore;
use std::collections::HashMap;
use tracing::{error, info};

struct Bot {
    guild_id: GuildId,
    channel_id: ChannelId,
}

#[derive(Clone, Debug)]
struct PantsOff {
    pub timestamp: Timestamp,
    pub proper: bool,
}

impl Bot {
    async fn score(&self, ctx: &Context, full: bool) -> String {
        let mut messages = self.channel_id.messages_iter(&ctx.http).boxed();

        let mut table = HashMap::new();

        let proper_regex = regex::Regex::new(r"(?i).*p.*a.*n.*t.*s.*o.*f.*f.*").unwrap();

        while let Some(Ok(message)) = messages.next().await {
            let author_entry = table.entry(message.author.id).or_insert(vec![]);

            author_entry.push(PantsOff {
                timestamp: message.timestamp,
                proper: proper_regex.is_match(&message.content),
            });
        }

        // sort each author's pants off by timestamp
        for (_author_id, pants_offs) in table.iter_mut() {
            pants_offs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        }

        let user_ids = table.keys().cloned().collect::<Vec<_>>();
        let names =
            futures::future::join_all(user_ids.iter().map(|user_id| user_id.to_user(&ctx.http)))
                .await;

        let mut user_names = HashMap::new();

        for (user_id, name) in user_ids.iter().zip(names.iter()) {
            user_names.insert(*user_id, name.as_ref().unwrap().name.clone());
        }

        #[derive(Default)]
        struct MonthScore {
            morning_pants_off: usize,
            evening_pants_off: usize,
            proper_pants_off: usize,
            missed_days: usize,
            infractions: usize,
            alternate_time_zones: usize,
        }

        let mut pretty_table = prettytable::Table::new();
        pretty_table.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

        pretty_table.set_titles(prettytable::row![
            "Month",
            "Morning",
            "Evening",
            "Proper",
            "Missed",
            "Infractions",
            "Alternate Time Zones"
        ]);

        // don't start collecting table until we've seen one pants off
        let mut has_seen_pants_off = false;

        let today = chrono::Utc::now().naive_utc();
        for year in 2020..=Timestamp::now().year() {
            for month in 1..=12 {
                let mut month_scores = HashMap::new();
                for day in 1..=31 {
                    if let Some(date) = chrono::NaiveDate::from_ymd_opt(year, month, day) {
                        if date > today.date() {
                            break;
                        }
                    } else {
                        // invalid date
                        continue;
                    };

                    for (author_id, pants_offs) in table.iter_mut() {
                        let partition_point = pants_offs.partition_point(|pants_off| {
                            pants_off.timestamp.year() == year
                                && pants_off.timestamp.month() as u32 == month
                                && pants_off.timestamp.day() == day
                        });

                        let today: Vec<_> = pants_offs.drain(..partition_point).collect();

                        let mut score = month_scores
                            .entry(*author_id)
                            .or_insert_with(MonthScore::default);

                        let mut morning = false;
                        let mut evening = false;

                        let mut morning_proper = false;
                        let mut evening_proper = false;

                        let mut alternate_time_zone = false;
                        let mut alternate_proper = false;

                        let mut saw_infraction = false;

                        for pants_off in today.iter() {
                            let timestamp = pants_off
                                .timestamp
                                .with_timezone(&chrono_tz::America::New_York);
                            if timestamp.minute() != 7 {
                                score.infractions += 1;
                                saw_infraction = true;
                                continue;
                            }

                            if timestamp.hour() == 6 {
                                morning = true;
                                if pants_off.proper {
                                    morning_proper = true;
                                }
                            }
                            // evening 6pm (UTC -> EST)
                            else if timestamp.hour() == 18 {
                                evening = true;
                                if pants_off.proper {
                                    evening_proper = true;
                                }
                            } else {
                                alternate_time_zone = true;
                                if pants_off.proper {
                                    alternate_proper = true;
                                }
                            }
                        }

                        has_seen_pants_off = has_seen_pants_off || !today.is_empty();

                        if !alternate_time_zone && !morning && !evening {
                            score.missed_days += 1;
                            if today.len() > 0 && !saw_infraction {
                                println!("Hmmm missed but {}", today.len());
                            }
                            continue;
                        }

                        if morning {
                            score.morning_pants_off += 1;
                        }

                        if evening {
                            score.evening_pants_off += 1;
                        }

                        if morning_proper {
                            score.proper_pants_off += 1;
                        }

                        if evening_proper {
                            score.proper_pants_off += 1;
                        }

                        if alternate_time_zone {
                            score.alternate_time_zones += 1;
                        }

                        if alternate_proper {
                            score.proper_pants_off += 1;
                        }
                    }
                }

                let is_future =
                    chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap() > today.date();

                if !has_seen_pants_off || is_future {
                    continue;
                }

                use num_traits::FromPrimitive;
                pretty_table.add_row(prettytable::row![format!(
                    "{}, {}",
                    chrono::Month::from_u32(month).unwrap().name(),
                    year
                )]);

                // add scores to table
                for (author_id, score) in month_scores.iter() {
                    pretty_table.add_row(prettytable::row![
                        user_names.get(author_id).unwrap(),
                        score.morning_pants_off,
                        score.evening_pants_off,
                        score.proper_pants_off,
                        score.missed_days,
                        score.infractions,
                        score.alternate_time_zones,
                    ]);
                }
            }
        }

        loop {
            let string = pretty_table.to_string();

            if !full && string.len() > 2000 {
                pretty_table.remove_row(0);
                continue;
            }

            return format!("\n```\n{}\n```", string);
        }
    }
}

#[async_trait]
impl EventHandler for Bot {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.content == "!hello" {
            if let Err(e) = msg.channel_id.say(&ctx.http, "world!").await {
                error!("Error sending message: {:?}", e);
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);

        let commands = GuildId::set_application_commands(&self.guild_id, &ctx.http, |commands| {
            commands.create_application_command(|command| {
                command
                    .name("score")
                    .description("post the pants-off score")
            })
        })
        .await
        .unwrap();

        info!("Successfully registered {:?} commands", commands);

        let score = self.score(&ctx, true).await;

        println!("Ran score: {}", score);
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let command = if let Interaction::ApplicationCommand(command) = interaction {
            command
        } else {
            return;
        };

        let response_content = match command.data.name.as_str() {
            "score" => "Counting...".to_owned(),

            command => unreachable!("Unknown command: {}", command),
        };

        let create_interaction_response =
            command.create_interaction_response(&ctx.http, |response| {
                response
                    .kind(InteractionResponseType::DeferredChannelMessageWithSource)
                    .interaction_response_data(|message| message.content(response_content))
            });

        if let Err(why) = create_interaction_response.await {
            eprintln!("Cannot respond to slash command: {}", why);
            return;
        }

        let typing = if let Ok(typing) = self.channel_id.start_typing(&ctx.http) {
            typing
        } else {
            eprintln!("Failed to start typing");
            return;
        };

        println!("Counting...");

        let score = self.score(&ctx, false).await;

        if typing.stop().is_none() {
            eprintln!("Failed to stop typing");
            return;
        }

        command
            .edit_original_interaction_response(&ctx.http, |response| {
                response.content(format!("{}", score))
            })
            .await
            .unwrap();
    }
}

#[shuttle_runtime::main]
async fn serenity(
    #[shuttle_secrets::Secrets] secret_store: SecretStore,
) -> shuttle_serenity::ShuttleSerenity {
    // Get the discord token set in `Secrets.toml`
    let token = if let Some(token) = secret_store.get("DISCORD_TOKEN") {
        token
    } else {
        return Err(anyhow!("'DISCORD_TOKEN' was not found").into());
    };

    let guild_id = if let Some(guild_id) = secret_store.get("GUILD_ID") {
        GuildId(guild_id.parse::<u64>().expect("Guild ID was not a number"))
    } else {
        return Err(anyhow!("'GUILD_ID' was not found").into());
    };

    let channel_id = if let Some(channel_id) = secret_store.get("PANTS_OFF_CHANNEL_ID") {
        ChannelId(
            channel_id
                .parse::<u64>()
                .expect("Channel ID was not a number"),
        )
    } else {
        return Err(anyhow!("'PANTS_OFF_CHANNEL_ID' was not found").into());
    };

    // Set gateway intents, which decides what events the bot will be notified about
    let intents = GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;

    let client = Client::builder(&token, intents)
        .event_handler(Bot {
            guild_id,
            channel_id,
        })
        .await
        .expect("Err creating client");

    Ok(client.into())
}
