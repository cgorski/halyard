// the prelude carries the router's and the head's everyday components and hooks
use halyard::{prelude::*, router::params::Params};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use thiserror::Error;

pub fn shell(options: HalyardOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    // Provides context that manages stylesheets, titles, meta tags, etc.
    provide_meta_context();
    let fallback = || view! { "Page not found." }.into_view();
    // a stand-in for a login: every page load starts logged in
    let is_admin = RwSignal::new(true);

    view! {
        <Stylesheet id="halyard" href="/pkg/ssr_modes.css"/>
        <Title text="Welcome to Halyard"/>
        <Meta name="color-scheme" content="dark light"/>
        <Router>
            <nav>
                <a href="/">"Home"</a>
                <a href="/admin">"Admin"</a>
                <button on:click=move |_| is_admin.update(|n| *n = !*n)>
                    {is_admin.map(|admin| if *admin { "Log Out" } else { "Log In" })}
                </button>
            </nav>
            <main>
                <FlatRoutes fallback>
                    // We’ll load the home page with out-of-order streaming and <Suspense/>
                    <Route path=StaticSegment("") view=HomePage/>

                    // We'll load the posts with async rendering, so they can set
                    // the title and metadata *after* loading the data
                    <Route
                        path=(StaticSegment("post"), ParamSegment("id"))
                        view=Post
                        ssr=SsrMode::Async
                    />
                    <Route
                        path=(StaticSegment("post_in_order"), ParamSegment("id"))
                        view=Post
                        ssr=SsrMode::InOrder
                    />
                    <Route
                        path=(StaticSegment("post_partially_blocked"), ParamSegment("id"))
                        view=Post
                    />
                    <ProtectedRoute
                        path=StaticSegment("admin")
                        view=Admin
                        ssr=SsrMode::Async
                        condition=move || is_admin.try_get()
                        redirect_path=|| "/"
                    />
                </FlatRoutes>
            </main>
        </Router>
    }
}

#[component]
fn HomePage() -> impl IntoView {
    // load the posts
    let posts = Resource::new(|| (), |_| list_post_metadata());
    let posts = move || posts.try_get().flatten().unwrap_or_default();

    let posts2 = Resource::new(|| (), |_| list_post_metadata());
    let posts2 =
        Resource::new(|| (), move |_| async move { posts2.await.len() });

    view! {
        <h1>"My Great Blog"</h1>
        <Suspense fallback=move || view! { <p>"Loading posts..."</p> }>
            <p>"number of posts: " {Suspend::new(async move { posts2.await })}</p>
        </Suspense>
        <Suspense fallback=move || view! { <p>"Loading posts..."</p> }>
            <ul>
                <For each=posts key=|post| post.id let:post>
                    <li>
                        <a href=format!("/post/{}", post.id)>{post.title.clone()}</a>
                        "|"
                        <a href=format!(
                            "/post_in_order/{}",
                            post.id,
                        )>{post.title.clone()} "(in order)"</a>
                        "|"
                        <a href=format!(
                            "/post_partially_blocked/{}",
                            post.id,
                        )>{post.title} "(partially blocked)"</a>
                    </li>
                </For>
            </ul>
        </Suspense>
    }
}

#[derive(Params, Copy, Clone, Debug, PartialEq, Eq)]
pub struct PostParams {
    id: Option<usize>,
}

#[component]
fn Post() -> impl IntoView {
    let query = use_params::<PostParams>();
    let id = move || {
        query.with(|q| {
            q.as_ref()
                .map(|q| q.id.unwrap_or_default())
                .map_err(|_| PostError::InvalidId)
        })
    };
    let post_resource = Resource::new_blocking(id, |id| async move {
        match id {
            Err(e) => Err(e),
            Ok(id) => get_post(id).await.ok_or(PostError::PostNotFound),
        }
    });
    let comments_resource = Resource::new(id, |id| async move {
        match id {
            Err(e) => Err(e),
            Ok(id) => Ok(get_comments(id).await),
        }
    });

    let post_view = Suspend::new(async move {
        match post_resource.await {
            Ok(post) => {
                Ok(view! {
                    <h1>{post.title.clone()}</h1>
                    <p>{post.content.clone()}</p>

                    // since we're using async rendering for this page,
                    // this metadata should be included in the actual HTML <head>
                    // when it's first served
                    <Title text=post.title/>
                    <Meta name="description" content=post.content/>
                })
            }
            Err(error) => Err(error),
        }
    });
    let comments_view = Suspend::new(async move {
        match comments_resource.await {
            Ok(comments) => Ok(view! {
                <h1>"Comments"</h1>
                <ul>
                    {comments
                        .into_iter()
                        .map(|comment| view! { <li>{comment}</li> })
                        .collect_view()}

                </ul>
            }),
            Err(error) => Err(error),
        }
    });

    view! {
        <em>"The world's best content."</em>
        <Suspense fallback=move || view! { <p>"Loading post..."</p> }>
            <ErrorBoundary fallback=|errors| {
                view! {
                    <div class="error">
                        <h1>"Something went wrong."</h1>
                        <ul>
                            {move || {
                                errors
                                    .get()
                                    .into_iter()
                                    .map(|(_, error)| view! { <li>{error.to_string()}</li> })
                                    .collect::<Vec<_>>()
                            }}

                        </ul>
                    </div>
                }
            }>{post_view}</ErrorBoundary>
        </Suspense>
        <Suspense fallback=move || view! { <p>"Loading comments..."</p> }>{comments_view}</Suspense>
    }
}

#[component]
pub fn Admin() -> impl IntoView {
    view! { <p>"You can only see this page if you're logged in."</p> }
}

// Dummy data, the same on the server and in the browser; the delays (on the server) stand
// in for a slow data source, to show the streaming modes

static POSTS: LazyLock<[Post; 3]> = LazyLock::new(|| {
    [
        Post {
            id: 0,
            title: "My first post".to_string(),
            content: "This is my first post".to_string(),
        },
        Post {
            id: 1,
            title: "My second post".to_string(),
            content: "This is my second post".to_string(),
        },
        Post {
            id: 2,
            title: "My third post".to_string(),
            content: "This is my third post".to_string(),
        },
    ]
});

#[derive(Error, Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostError {
    #[error("Invalid post ID.")]
    InvalidId,
    #[error("Post not found.")]
    PostNotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    id: usize,
    title: String,
    content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostMetadata {
    id: usize,
    title: String,
}

async fn delay(seconds: u64) {
    #[cfg(feature = "ssr")]
    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    #[cfg(not(feature = "ssr"))]
    let _ = seconds;
}

pub async fn list_post_metadata() -> Vec<PostMetadata> {
    delay(1).await;
    POSTS
        .iter()
        .map(|data| PostMetadata {
            id: data.id,
            title: data.title.clone(),
        })
        .collect()
}

pub async fn get_post(id: usize) -> Option<Post> {
    delay(1).await;
    POSTS.iter().find(|post| post.id == id).cloned()
}

pub async fn get_comments(id: usize) -> Vec<String> {
    delay(2).await;
    _ = id;
    vec!["Some comment".into(), "Some other comment".into()]
}
