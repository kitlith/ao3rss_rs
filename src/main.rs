#![feature(proc_macro_hygiene, decl_macro)]

use std::{collections::HashMap, error::Error};

use futures_core::{future::FusedFuture, stream::Stream};
use futures_util::future::{FutureExt, TryFutureExt};
use futures_util::{pin_mut, select};

use async_stream::stream;

use hyper::body::{Body, Bytes};
use tokio::time::{interval, Duration};
use warp::http::Response;
use warp::Filter;

use chrono::NaiveDate;

use select::document::Document;
use select::predicate::{Attr, Class, Name, Predicate};

// scraping information from:
//  - archiveofourown.org/works/<id>/navigate (individial chapter dates!)
//  - archiveofourown.org/works/<id>?view_full_work=true (summary/chapter notes/content)

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

fn parse_date_from_node(node: select::node::Node) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&node.text(), "%Y-%m-%d").ok()
}

async fn scrape_work(work_id: u64) -> Result<Work, Box<dyn Error + Send + Sync>> {
    let client = reqwest::Client::new();

    /*let navigation = client
    .get(&format!("https://archiveofourown.org/works/{}/navigate", work_id))
    .send()
    .and_then(|x| x.text())
    .map_ok(|x| Document::from(x.as_str()));*/
    let work = client
        .get(&format!(
            "https://archiveofourown.org/works/{}?view_full_work=true",
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
            .map(|chapter_node| {
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
            })
            .collect::<Result<Vec<Chapter>, Box<dyn Error + Send + Sync>>>()?,
    })
}

fn keepalive_future(
    fut: impl FusedFuture<Output = Result<Bytes, Box<dyn Error + Send + Sync>>>,
) -> impl Stream<Item = Result<Bytes, Box<dyn Error + Send + Sync>>> {
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

fn rfc822(date: NaiveDate) -> String {
    date.format("%a, %d %b %Y 00:00:00 +0000").to_string()
}

#[tokio::main]
async fn main() {
    let work = warp::path!("work" / u64).and(warp::get()).map(|work_id| {
        let fut = scrape_work(work_id).map_ok(|res| {
            // generate rss feed
            // TODO: categories/tags?
            let channel = rss::ChannelBuilder::default()
                .title(res.title)
                .description(res.summary.unwrap_or("AO3 Fic".to_string()))
                .link(format!("https://archiveofourown.org/works/{}", res.work_id))
                .pub_date(rfc822(res.publish_date))
                .last_build_date(rfc822(res.update_date))
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
                    res.chapters
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
                                ) // TODO: unwrap
                                .build()
                                .unwrap() // TODO: unwrap
                        })
                        .collect::<Vec<_>>(),
                )
                .build()
                .unwrap(); // TODO: unwrap

            // TODO: validate?

            let mut res = Vec::new();
            channel.write_to(&mut res).unwrap();

            Bytes::from(res)
        });
        let stream = keepalive_future(fut.fuse());
        let resp = Response::new(Body::wrap_stream(stream));
        resp
    });

    warp::serve(work).run(([127, 0, 0, 1], 3336)).await;
}
