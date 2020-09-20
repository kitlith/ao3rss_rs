#![feature(proc_macro_hygiene, decl_macro)]

use std::{collections::HashMap, error::Error};

use futures_core::{future::FusedFuture, stream::Stream};
use futures_util::future::{FutureExt, TryFutureExt};
use futures_util::{pin_mut, select};

use async_stream::stream;

use hyper::body::Bytes;
use tokio::time::{interval, Duration};

use chrono::NaiveDate;

use select::document::Document;
use select::predicate::{Attr, Class, Name, Predicate};

use serde::Deserialize;

// scraping information from:
//  - archiveofourown.org/works/<id>?view_adult=true&view_full_work=true (summary/chapter notes/content)
//  - maybe archiveofourown.org/works/<id>/navigate (individial chapter dates/what original ao3rss used)

struct Work {
    work_id: u64,
    title: String,
    summary: Option<String>,
    notes: Option<String>,
    post_notes: Option<String>,
    publish_date: NaiveDate,
    update_date: NaiveDate,
    chapters: Vec<Chapter>,
}

struct Chapter {
    title: String,
    link: String,
    summary: Option<String>,
    //update_date: NaiveDate,
    pre_notes: Option<String>,
    content: String,
    post_notes: Option<String>,
}

impl Work {
    async fn scrape(client: &reqwest::Client, work_id: u64) -> Result<Work, Box<dyn Error + Send + Sync>> {
        /*let navigation = client
        .get(&format!("https://archiveofourown.org/works/{}/navigate", work_id))
        .send()
        .and_then(|x| x.text())
        .map_ok(|x| Document::from(x.as_str()));*/
        let work = client
            .get(&format!(
                "https://archiveofourown.org/works/{}?view_adult=true&view_full_work=true",
                work_id
            ))
            .send()
            .and_then(|x| x.text())
            .map_ok(|x| Document::from(x.as_str()));
        let work = work.await?;

        Ok(Work {
            work_id,
            title: work
                .find(Name("h2").and(Class("title")))
                .nth(0)
                .map(|n| n.text())
                .ok_or("Missing Work Title")?,
            summary: work
                .find(
                    Attr("id", "workskin")
                        .child(Class("preface"))
                        .descendant(Class("userstuff")),
                )
                .nth(0)
                .map(|n| n.text()),
            notes: None,      // TODO?
            post_notes: None, // TODO?
            publish_date: work
                .find(Class("published").and(Name("dd")))
                .nth(0)
                .and_then(parse_date_from_node)
                .ok_or("Missing Publish Date")?,
            update_date: work
                .find(Class("status").and(Name("dd")))
                .nth(0)
                .and_then(parse_date_from_node)
                .ok_or("Missing Update Date")?,
            chapters: work
                .find(Attr("id", "chapters").child(Class("chapter")))
                .map(|chapter_node| Chapter::scrape(chapter_node))
                .collect::<Result<Vec<Chapter>, Box<dyn Error + Send + Sync>>>()?,
        })
    }

    fn to_rss(self) -> rss::Channel {
        // NOTE: This function makes use of unwrap after building all the items,
        // but this will apparently never panic because all fields have default values.
        rss::ChannelBuilder::default()
            .title(self.title)
            .description(self.summary.unwrap_or("AO3 Fic".to_string()))
            .link(format!("https://archiveofourown.org/works/{}", self.work_id))
            .pub_date(rfc822(self.publish_date))
            .last_build_date(rfc822(self.update_date))
            .generator(Some("https://github.com/kitlith/ao3rss_rs".to_string()))
            .namespaces(
                [(
                    "content".to_string(),
                    "http://purl.org/rss/1.0/modules/content/".to_string(),
                )]
                    .iter()
                    .cloned()
                    .collect::<HashMap<String, String>>(),
            )
            .items(
                self.chapters
                    .into_iter()
                    .map(|chapter| {
                        rss::ItemBuilder::default()
                            .title(chapter.title)
                            .link(chapter.link.clone())
                            .description(chapter.summary)
                            .content(chapter.content)
                            .guid(
                                rss::GuidBuilder::default()
                                    .value(chapter.link)
                                    .permalink(true)
                                    .build()
                                    .unwrap(),
                            )
                            .build()
                            .unwrap()
                    })
                    .collect::<Vec<_>>(),
            )
            .build()
            .unwrap()
    }
}



impl Chapter {
    fn scrape(chapter_node: select::node::Node) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let title = chapter_node
            .find(Class("title"))
            .nth(0)
            .ok_or("Missing Chapter Title")?;
        let chapter_link = title.find(Name("a")).nth(0).ok_or("Missing Chapter Link")?;

        let title = title.text();
        let chapter_link = chapter_link.attr("href").ok_or("Missing href?")?;

        Ok(Chapter {
            title,
            link: format!("https://archiveofourown.org{}", chapter_link),
            summary: chapter_node
                .find(Class("summary").child(Class("userstuff")))
                .nth(0)
                .map(|node| node.inner_html()), // TODO: consider dropping the inner
            pre_notes: None,
            content: chapter_node
                .children()
                .find(|node| node.is(Class("userstuff")))
                .map(|node| node.inner_html())
                .ok_or("Missing content")?,
            post_notes: None,
        })
    }
}

fn parse_date_from_node(node: select::node::Node) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&node.text(), "%Y-%m-%d").ok()
}

fn keepalive_future<E: Unpin>(
    fut: impl FusedFuture<Output = Result<Bytes, E>>,
) -> impl Stream<Item = Result<Bytes, E>> {
    stream! {
        let mut interval = interval(Duration::from_millis(1000)); // every second
        pin_mut!(fut);
        loop {
            let next;
            let mut exit = false;
            select! {
                _ = interval.tick().fuse() => next = Ok(Bytes::from("<!-- keepalive -->")),
                res = &mut fut => {
                    next = res;
                    exit = true;
                }
            }

            yield next;
            if exit {
                break;
            }
        }
    }
}

#[derive(Deserialize)]
struct Token {
    token: String
}

#[allow(unused)]
async fn ao3_login(client: &reqwest::Client, username: &str, password: &str) -> Result<bool, reqwest::Error> {
    let token: Token = client.get("https://archiveofourown.org/token_dispenser.json")
        .send()
        .await?
        .json()
        .await?;

    let params = [
        ("utf8", "✓"),
        ("authenticity_token", &token.token),
        ("user[login]", username),
        ("user[password]", password),
        ("user[remember_me]", "1"),
        ("commit", "Log in")
    ];

    let status = client.post("https://archiveofourown.org/users/login")
        .form(&params)
        .send()
        .await?
        .status();

    // TODO: this is apparently incorrect
    Ok(status == reqwest::StatusCode::FOUND)
}

fn rfc822(date: NaiveDate) -> String {
    date.format("%a, %d %b %Y 00:00:00 +0000").to_string()
}

#[cfg(feature = "warp")]
#[tokio::main]
async fn main() {
    use warp::http::Response;
    use warp::Filter;
    use hyper::body::Body;

    let client = reqwest::ClientBuilder::new()
        .user_agent(rocket_routes::APP_USER_AGENT)
        .build().unwrap();

    // TODO: login

    let work = warp::path!("work" / u64).and(warp::get()).map(|work_id| {
        let fut = Work::scrape(&client, work_id).map_ok(|work| {
            // generate rss feed
            // TODO: categories/tags?
            let channel = work.to_rss();

            // TODO: validate?

            let mut res = Vec::new();
            channel.write_to(&mut res).unwrap();

            Bytes::from(res)
        });
        let stream = keepalive_future(fut.fuse());

        Response::builder()
            .header("Content-Type", "application/rss+xml")
            .body(Body::wrap_stream(stream))
            .unwrap() // this should not panic, as the builder should not be given any invalid data.
    });

    warp::serve(work).run(([127, 0, 0, 1], 3336)).await;
}

#[cfg(feature = "rocket")]
mod rocket_routes {
    use rocket::{get, State};
    use rocket::response::content::Xml;
    use rocket::response::{Stream, Responder};
    use super::*;

    pub static APP_USER_AGENT: &str = concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION"),
    );

    // long return type :(
    // Result<Xml<Stream<StreamReader<impl futures_core::Stream<Item = Result<Bytes, std::io::Error>>, Bytes>>>, Debug<std::io::Error>>
    #[get("/work/<work_id>")]
    pub async fn work<'r>(work_id: u64, client: State<'r, reqwest::Client>) -> impl Responder<'r, 'r> {
        let fut = Work::scrape(client.inner(), work_id).map_ok(|work| {
            // generate rss feed
            // TODO: categories/tags?
            let channel = work.to_rss();

            // TODO: validate?

            let mut res = Vec::new();
            channel.write_to(&mut res).unwrap();

            Bytes::from(res)
        }).map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err));

        let stream = keepalive_future(fut.fuse());
        let reader = tokio::io::stream_reader(stream);

        Xml(Stream::chunked(reader, rocket::response::DEFAULT_CHUNK_SIZE))
    }
}

#[cfg(feature = "rocket")]
#[rocket::launch]
fn launch() -> rocket::Rocket {
    use rocket::routes;

    rocket::ignite()
        .mount("/", routes![rocket_routes::work])
        .attach(rocket::fairing::AdHoc::on_attach("Client Config", |mut rocket| async {
            let client = reqwest::ClientBuilder::new()
                .user_agent(rocket_routes::APP_USER_AGENT)
                .cookie_store(true)
                .build().unwrap();

            let config = rocket.config().await;

            use rocket::config::ConfigError;

            // TEMPORARY -- it's not great that the password gets printed to console.
            match (config.get_str("ao3_username"), config.get_str("ao3_password")) {
                // if either one of them are missing, just ignore it
                (Err(ConfigError::Missing(_)), _) => (),
                (_, Err(ConfigError::Missing(_))) => (),

                // if both are present, attempt to login
                (Ok(username), Ok(password)) => {
                    println!("Found login information!");
                    let logged_in = ao3_login(&client, username, password).await.unwrap();

                    if logged_in {
                        println!("Successfully logged in!");
                    } else {
                        println!("Failed to log in.");
                    }
                },

                // otherwise crash
                (e1, e2) => {
                    e1.unwrap(); e2.unwrap();
                }
            }

            Ok(rocket.manage(client))
        }))
}