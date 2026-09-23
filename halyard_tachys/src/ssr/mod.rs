use crate::{
    html::attribute::any_attribute::AnyAttribute,
    view::{Position, RenderHtml},
    view_error::{report, report_once, ViewError},
};
use futures::Stream;
use std::{
    collections::VecDeque,
    fmt::{Debug, Write},
    future::Future,
    mem,
    pin::Pin,
    sync::{atomic::AtomicBool, Arc},
    task::{Context, Poll},
};

/// Manages streaming HTML rendering for the response to a single request.
#[derive(Default)]
pub struct StreamBuilder {
    pub(crate) sync_buf: String,
    pub(crate) chunks: VecDeque<StreamChunk>,
    pending: Option<ChunkFuture>,
    pending_ooo: VecDeque<PinnedFuture<OooChunk>>,
    id: Option<Vec<u16>>,
}

type PinnedFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type ChunkFuture = PinnedFuture<VecDeque<StreamChunk>>;

impl StreamBuilder {
    /// Creates a new HTML stream.
    pub fn new(id: Option<Vec<u16>>) -> Self {
        Self::with_capacity(0, id)
    }

    /// Creates a new stream with a given capacity in the synchronous buffer and an identifier.
    ///
    /// The capacity is a hint (a view's length estimate): if it cannot be allocated, the
    /// buffer starts empty and grows as it is written.
    pub fn with_capacity(capacity: usize, id: Option<Vec<u16>>) -> Self {
        let mut builder = Self {
            id,
            ..Default::default()
        };
        builder.reserve(capacity);
        builder
    }

    /// Reserves additional space in the synchronous buffer.
    ///
    /// A hint: if it cannot be allocated, the buffer grows as it is written instead.
    pub fn reserve(&mut self, additional: usize) {
        _ = self.sync_buf.try_reserve(additional);
    }

    /// Pushes text into the synchronous buffer.
    pub fn push_sync(&mut self, string: &str) {
        self.sync_buf.push_str(string);
    }

    /// Pushes an async block into the stream.
    pub fn push_async(
        &mut self,
        fut: impl Future<Output = VecDeque<StreamChunk>> + Send + 'static,
    ) {
        // flush sync chunk
        let sync = mem::take(&mut self.sync_buf);
        if !sync.is_empty() {
            self.chunks.push_back(StreamChunk::Sync(sync));
        }
        self.chunks.push_back(StreamChunk::Async {
            chunks: Box::pin(fut) as PinnedFuture<VecDeque<StreamChunk>>,
        });
    }

    /// Mutates the synchronous buffer.
    pub fn with_buf(&mut self, fun: impl FnOnce(&mut String)) {
        fun(&mut self.sync_buf)
    }

    /// Takes all chunks currently available in the stream, including the synchronous buffer.
    pub fn take_chunks(&mut self) -> VecDeque<StreamChunk> {
        let sync = mem::take(&mut self.sync_buf);
        if !sync.is_empty() {
            self.chunks.push_back(StreamChunk::Sync(sync));
        }
        mem::take(&mut self.chunks)
    }

    /// Appends another stream to this one.
    pub fn append(&mut self, mut other: StreamBuilder) {
        if !self.sync_buf.is_empty() {
            self.chunks
                .push_back(StreamChunk::Sync(mem::take(&mut self.sync_buf)));
        }
        self.chunks.append(&mut other.chunks);
        self.sync_buf.push_str(&other.sync_buf);
    }

    /// Completes the stream.
    pub fn finish(mut self) -> Self {
        let sync_buf_remaining = mem::take(&mut self.sync_buf);
        if sync_buf_remaining.is_empty() {
            return self;
        } else if let Some(StreamChunk::Sync(buf)) = self.chunks.back_mut() {
            buf.push_str(&sync_buf_remaining);
        } else {
            self.chunks.push_back(StreamChunk::Sync(sync_buf_remaining));
        }
        self
    }

    // Out-of-Order Streaming
    /// Pushes a fallback for out-of-order streaming.
    pub fn push_fallback<View>(
        &mut self,
        fallback: View,
        position: &mut Position,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) where
        View: RenderHtml,
    {
        self.write_chunk_marker(true);
        fallback.to_html_with_buf(
            &mut self.sync_buf,
            position,
            true,
            mark_branches,
            extra_attrs,
        );
        self.write_chunk_marker(false);
        *position = Position::NextChild;
    }

    /// Increments the chunk ID.
    ///
    /// After the largest id (65535) it wraps around to 0, logged once: ids only have to
    /// differ between chunks that are pending at the same time, and a chunk that has been
    /// streamed has removed its markers.
    pub fn next_id(&mut self) {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        if let Some(last) = self.id.as_mut().and_then(|ids| ids.last_mut()) {
            *last = last.checked_add(1).unwrap_or_else(|| {
                report_once(&REPORTED, &ViewError::ChunkIdWrapped);
                0
            });
        }
    }

    /// Returns the current ID.
    pub fn clone_id(&self) -> Option<Vec<u16>> {
        self.id.clone()
    }

    /// Returns an ID that is a child of the current one.
    pub fn child_id(&self) -> Option<Vec<u16>> {
        let mut child = self.id.clone();
        if let Some(child) = child.as_mut() {
            child.push(0);
        }
        child
    }

    /// Inserts a marker for the current out-of-order chunk.
    pub fn write_chunk_marker(&mut self, opening: bool) {
        if let Some(id) = &self.id {
            _ = self
                .sync_buf
                .try_reserve(id.len().saturating_mul(2).saturating_add(11));
            self.sync_buf.push_str("<!--s-");
            push_chunk_id(&mut self.sync_buf, id);
            if opening {
                self.sync_buf.push_str("o-->");
            } else {
                self.sync_buf.push_str("c-->");
            }
        }
    }

    /// Injects an out-of-order chunk into the stream.
    pub fn push_async_out_of_order<View>(
        &mut self,
        view: impl Future<Output = Option<View>> + Send + 'static,
        position: &mut Position,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) where
        View: RenderHtml,
    {
        self.push_async_out_of_order_with_nonce(
            view,
            position,
            mark_branches,
            None,
            extra_attrs,
        );
    }

    /// Injects an out-of-order chunk into the stream, using the given nonce for `<script>` tags.
    pub fn push_async_out_of_order_with_nonce<View>(
        &mut self,
        view: impl Future<Output = Option<View>> + Send + 'static,
        position: &mut Position,
        mark_branches: bool,
        nonce: Option<Arc<str>>,
        extra_attrs: Vec<AnyAttribute>,
    ) where
        View: RenderHtml,
    {
        let id = self.clone_id();
        // copy so it's not updated by additional iterations
        // i.e., restart in the same position we were at when we suspended
        let mut position = *position;

        self.chunks.push_back(StreamChunk::OutOfOrder {
            chunks: Box::pin(async move {
                let view = view.await;

                let mut subbuilder = StreamBuilder::new(id);
                let mut id = String::new();
                if let Some(ids) = &subbuilder.id {
                    push_chunk_id(&mut id, ids);
                }
                if let Some(id) = subbuilder.id.as_mut() {
                    id.push(0);
                }
                let replace = view.is_some();
                view.to_html_async_with_buf::<true>(
                    &mut subbuilder,
                    &mut position,
                    true,
                    mark_branches,
                    extra_attrs,
                );
                let chunks = subbuilder.finish().take_chunks();
                let mut flattened_chunks =
                    VecDeque::with_capacity(chunks.len());
                for chunk in chunks {
                    // this will wait for any ErrorBoundary async nodes and flatten them out
                    if let StreamChunk::Async { chunks } = chunk {
                        flattened_chunks.extend(chunks.await);
                    } else {
                        flattened_chunks.push_back(chunk);
                    }
                }

                OooChunk {
                    id,
                    chunks: flattened_chunks,
                    replace,
                    nonce,
                }
            }),
        });
    }
}

/// Writes an out-of-order chunk id as the markers and the replacement script spell it:
/// `{piece}-` for each piece.
fn push_chunk_id(buf: &mut String, id: &[u16]) {
    for piece in id {
        // writing to a `String` cannot fail
        _ = write!(buf, "{piece}-");
    }
}

/// Splits `buf` around the fallback of the out-of-order chunk `id`: what comes before its
/// opening marker, and what comes after the closing marker that follows it.
///
/// `None` if the opening marker is not in `buf` (it was streamed already), or if no closing
/// marker follows it (logged: the markers are malformed).
fn split_around_placeholder<'a>(
    buf: &'a str,
    id: &str,
) -> Option<(&'a str, &'a str)> {
    let opening = format!("<!--s-{id}o-->");
    let closing = format!("<!--s-{id}c-->");
    let (before, from_opening) = buf.split_once(&opening)?;
    match from_opening.split_once(&closing) {
        Some((_fallback, after)) => Some((before, after)),
        None => {
            report(&ViewError::UnclosedChunkMarker { id: id.to_string() });
            None
        }
    }
}

impl Debug for StreamBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamBuilderInner")
            .field("sync_buf", &self.sync_buf)
            .field("chunks", &self.chunks)
            .field("pending", &self.pending.is_some())
            .finish()
    }
}

/// A chunk of the HTML stream.
pub enum StreamChunk {
    /// Some synchronously-available HTML.
    Sync(String),
    /// The chunk can be rendered asynchronously in order.
    Async {
        /// A collection of in-order chunks.
        chunks: PinnedFuture<VecDeque<StreamChunk>>,
    },
    /// The chunk can be rendered asynchronously out of order.
    OutOfOrder {
        /// A collection of out-of-order chunks
        chunks: PinnedFuture<OooChunk>,
    },
}

/// A chunk of the out-of-order stream.
#[derive(Debug)]
pub struct OooChunk {
    id: String,
    chunks: VecDeque<StreamChunk>,
    replace: bool,
    nonce: Option<Arc<str>>,
}

impl OooChunk {
    /// Pushes an opening `<template>` tag into the buffer.
    pub fn push_start(id: &str, buf: &mut String) {
        buf.push_str("<template id=\"");
        buf.push_str(id);
        buf.push('f');
        buf.push_str("\">");
    }

    /// Pushes a closing `</template>` and update script into the buffer.
    pub fn push_end(replace: bool, id: &str, buf: &mut String) {
        Self::push_end_with_nonce(replace, id, buf, None);
    }

    /// Pushes a closing `</template>` and update script with the given nonce into the buffer.
    pub fn push_end_with_nonce(
        replace: bool,
        id: &str,
        buf: &mut String,
        nonce: Option<&str>,
    ) {
        buf.push_str("</template>");

        if let Some(nonce) = nonce {
            buf.push_str("<script nonce=\"");
            buf.push_str(nonce);
            buf.push_str(r#"">(function() { let id = ""#);
        } else {
            buf.push_str(r#"<script>(function() { let id = ""#);
        }
        buf.push_str(id);
        buf.push_str(
            "\";let open = undefined;let close = undefined;let walker = \
             document.createTreeWalker(document.body, \
             NodeFilter.SHOW_COMMENT);while(walker.nextNode()) \
             {if(walker.currentNode.textContent == `s-${id}o`){ \
             open=walker.currentNode; } else \
             if(walker.currentNode.textContent == `s-${id}c`) { close = \
             walker.currentNode;}}let range = new Range(); \
             range.setStartBefore(open); range.setEndBefore(close);",
        );
        if replace {
            buf.push_str(
                "range.deleteContents(); let tpl = \
                 document.getElementById(`${id}f`); \
                 close.parentNode.insertBefore(tpl.content.cloneNode(true), \
                 close);close.remove();",
            );
        } else {
            buf.push_str("close.remove();open.remove();");
        }
        buf.push_str("})()</script>");
    }

    /// Consumes this structure and returns its inner chunks of the stream.
    pub fn take_chunks(self) -> VecDeque<StreamChunk> {
        self.chunks
    }
}

impl Debug for StreamChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sync(arg0) => f.debug_tuple("Sync").field(arg0).finish(),
            Self::Async { .. } => {
                f.debug_struct("Async").finish_non_exhaustive()
            }
            Self::OutOfOrder { .. } => {
                f.debug_struct("OutOfOrder").finish_non_exhaustive()
            }
        }
    }
}

impl Stream for StreamBuilder {
    type Item = String;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut this = self.as_mut();
        let pending = this.pending.take();
        if let Some(mut pending) = pending {
            match pending.as_mut().poll(cx) {
                Poll::Pending => {
                    this.pending = Some(pending);
                    Poll::Pending
                }
                Poll::Ready(chunks) => {
                    for chunk in chunks.into_iter().rev() {
                        this.chunks.push_front(chunk);
                    }
                    self.poll_next(cx)
                }
            }
        } else {
            let next_chunk = this.chunks.pop_front();
            match next_chunk {
                None => {
                    if this.pending_ooo.is_empty() {
                        if this.sync_buf.is_empty() {
                            Poll::Ready(None)
                        } else {
                            Poll::Ready(Some(mem::take(&mut this.sync_buf)))
                        }
                    } else {
                        // check if *any* pending out-of-order chunk is ready
                        for mut chunk in mem::take(&mut this.pending_ooo) {
                            match chunk.as_mut().poll(cx) {
                                Poll::Ready(OooChunk {
                                    id,
                                    chunks,
                                    replace,
                                    nonce,
                                }) => {
                                    if let Some((before, after)) =
                                        split_around_placeholder(
                                            &this.sync_buf,
                                            &id,
                                        )
                                    {
                                        let chunks_iter =
                                            chunks.into_iter().rev();

                                        // TODO can probably make this more efficient
                                        let mut buf = String::new();
                                        buf.push_str(before);

                                        let mut held_chunks = VecDeque::new();
                                        for chunk in chunks_iter {
                                            if let StreamChunk::Sync(ready) =
                                                chunk
                                            {
                                                buf.push_str(&ready);
                                            } else {
                                                held_chunks.push_front(chunk);
                                            }
                                        }
                                        buf.push_str(after);
                                        this.sync_buf = buf;
                                        for chunk in held_chunks {
                                            this.chunks.push_front(chunk);
                                        }
                                    } else {
                                        OooChunk::push_start(
                                            &id,
                                            &mut this.sync_buf,
                                        );
                                        for chunk in chunks.into_iter().rev() {
                                            if let StreamChunk::Sync(ready) =
                                                chunk
                                            {
                                                this.sync_buf.push_str(&ready);
                                            } else {
                                                this.chunks.push_front(chunk);
                                            }
                                        }
                                        OooChunk::push_end_with_nonce(
                                            replace,
                                            &id,
                                            &mut this.sync_buf,
                                            nonce.as_deref(),
                                        );
                                    }
                                }
                                Poll::Pending => {
                                    this.pending_ooo.push_back(chunk);
                                }
                            }
                        }

                        if this.sync_buf.is_empty() {
                            Poll::Pending
                        } else {
                            Poll::Ready(Some(mem::take(&mut this.sync_buf)))
                        }
                    }
                }
                Some(StreamChunk::Sync(value)) => {
                    this.sync_buf.push_str(&value);
                    loop {
                        match this.chunks.pop_front() {
                            None => break,
                            Some(StreamChunk::Async { chunks }) => {
                                this.chunks
                                    .push_front(StreamChunk::Async { chunks });
                                break;
                            }
                            Some(StreamChunk::OutOfOrder {
                                chunks, ..
                            }) => {
                                this.pending_ooo.push_back(chunks);
                                break;
                            }
                            Some(StreamChunk::Sync(next)) => {
                                this.sync_buf.push_str(&next);
                            }
                        }
                    }

                    this.poll_next(cx)
                }
                Some(StreamChunk::Async { chunks, .. }) => {
                    this.pending = Some(chunks);
                    if this.sync_buf.is_empty() {
                        self.poll_next(cx)
                    } else {
                        Poll::Ready(Some(mem::take(&mut this.sync_buf)))
                    }
                }
                Some(StreamChunk::OutOfOrder { chunks, .. }) => {
                    this.pending_ooo.push_back(chunks);
                    if this.sync_buf.is_empty() {
                        self.poll_next(cx)
                    } else {
                        Poll::Ready(Some(mem::take(&mut this.sync_buf)))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StreamBuilder;
    use crate::view::Position;
    use futures::{executor::block_on, StreamExt};

    /// Streams what `before` writes, then an out-of-order chunk (for id `0-`) that resolves
    /// to `resolved`, as a `<Suspense>` does after writing its fallback.
    fn stream(before: impl FnOnce(&mut StreamBuilder)) -> String {
        let mut builder = StreamBuilder::new(Some(vec![0]));
        before(&mut builder);
        builder.push_async_out_of_order(
            async { Some("resolved") },
            &mut Position::NextChild,
            false,
            vec![],
        );
        block_on(builder.finish().collect::<Vec<_>>()).concat()
    }

    #[test]
    fn an_out_of_order_chunk_replaces_its_fallback_in_the_buffer() {
        let html = stream(|builder| {
            builder.push_sync("<p>");
            builder.push_fallback(
                "loading",
                &mut Position::NextChild,
                false,
                vec![],
            );
            builder.push_sync("</p>");
        });
        assert_eq!(html, "<p>resolved</p>");
    }

    /// Raw HTML (`inner_html`) can hold a comment that looks like a chunk's opening marker.
    /// With no closing marker after it, the stream unwrapped `None` and failed the request.
    #[test]
    fn an_opening_marker_without_a_closing_marker_streams_the_chunk_as_a_template(
    ) {
        let html =
            stream(|builder| builder.push_sync("<div><!--s-0-o--></div>"));
        assert!(html.starts_with("<div><!--s-0-o--></div>"), "{html}");
        assert!(html.contains("<template id=\"0-f\">resolved"), "{html}");
    }

    /// A look-alike closing marker in front of the opening marker made the replaced range
    /// negative: an overflow (a panic in debug builds, a slice out of bounds in release).
    #[test]
    fn a_closing_marker_before_the_opening_marker_does_not_confuse_the_replacement(
    ) {
        let html = stream(|builder| {
            builder.push_sync("<!--s-0-c-->");
            builder.push_fallback(
                "loading",
                &mut Position::NextChild,
                false,
                vec![],
            );
        });
        assert_eq!(html, "<!--s-0-c-->resolved");
    }

    /// The 65536th out-of-order chunk at one level overflowed the `u16` id.
    #[test]
    fn next_id_wraps_at_the_largest_id_instead_of_overflowing() {
        let mut builder = StreamBuilder::new(Some(vec![3, u16::MAX]));
        builder.next_id();
        assert_eq!(builder.clone_id(), Some(vec![3, 0]));
    }

    #[test]
    fn next_id_increments_the_last_piece() {
        let mut builder = StreamBuilder::new(Some(vec![3, 7]));
        builder.next_id();
        assert_eq!(builder.clone_id(), Some(vec![3, 8]));
        assert_eq!(builder.child_id(), Some(vec![3, 8, 0]));

        let mut no_id = StreamBuilder::new(None);
        no_id.next_id();
        assert_eq!(no_id.clone_id(), None);
    }

    /// A view's length estimate is used as the initial capacity of the stream's buffer; an
    /// estimate that cannot be allocated panicked ("capacity overflow").
    #[test]
    fn a_capacity_that_cannot_be_allocated_leaves_the_buffer_empty() {
        let mut builder = StreamBuilder::with_capacity(usize::MAX, None);
        builder.reserve(usize::MAX);
        builder.push_sync("fits");
        assert_eq!(builder.sync_buf, "fits");
    }

    /// A view's length estimate is the initial capacity of the HTML buffer; an estimate
    /// that cannot be allocated panicked ("capacity overflow").
    #[test]
    fn a_view_that_estimates_usize_max_renders_and_streams() {
        use crate::{view::RenderHtml, view_error::test_support::HugeView};

        assert_eq!(HugeView.to_html(), "huge");
        assert_eq!(HugeView.to_html_branching(), "huge");
        let in_order = HugeView.to_html_stream_in_order();
        assert_eq!(block_on(in_order.collect::<Vec<_>>()).concat(), "huge");
        let out_of_order = HugeView.to_html_stream_out_of_order();
        assert_eq!(block_on(out_of_order.collect::<Vec<_>>()).concat(), "huge");
    }

    #[test]
    fn chunk_markers_spell_every_id_piece() {
        let mut builder = StreamBuilder::new(Some(vec![1, 22]));
        builder.write_chunk_marker(true);
        builder.write_chunk_marker(false);
        assert_eq!(builder.sync_buf, "<!--s-1-22-o--><!--s-1-22-c-->");
    }
}

/*
#[cfg(test)]
mod tests {
    use crate::{
        async_views::{FutureViewExt, Suspend},
        html::element::{em, main, p, ElementChild, HtmlElement, Main},
        renderer::dom::Dom,
        view::RenderHtml,
    };
    use futures::StreamExt;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn in_order_stream_of_sync_content_ready_immediately() {
        let el: HtmlElement<Main, _, _, Dom> = main().child(p().child((
            "Hello, ",
            em().child("beautiful"),
            " world!",
        )));
        let mut stream = el.to_html_stream_in_order();

        let html = stream.next().await.unwrap();
        assert_eq!(
            html,
            "<main><p>Hello, <em>beautiful</em> world!</p></main>"
        );
    }

    #[tokio::test]
    async fn in_order_single_async_block_in_stream() {
        let el = async {
            sleep(Duration::from_millis(250)).await;
            "Suspended"
        }
        .suspend();
        let mut stream =
            <Suspend<false, _, _> as RenderHtml<Dom>>::to_html_stream_in_order(
                el,
            );

        let html = stream.next().await.unwrap();
        assert_eq!(html, "Suspended<!>");
    }

    #[tokio::test]
    async fn in_order_async_with_siblings_in_stream() {
        let el = (
            "Before Suspense",
            async {
                sleep(Duration::from_millis(250)).await;
                "Suspended"
            }
            .suspend(),
        );
        let mut stream =
            <(&str, Suspend<false, _, _>) as RenderHtml<Dom>>::to_html_stream_in_order(
                el,
            );

        assert_eq!(stream.next().await.unwrap(), "Before Suspense");
        assert_eq!(stream.next().await.unwrap(), "<!>Suspended");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn in_order_async_inside_element_in_stream() {
        let el: HtmlElement<_, _, _, Dom> = p().child((
            "Before Suspense",
            async {
                sleep(Duration::from_millis(250)).await;
                "Suspended"
            }
            .suspend(),
        ));
        let mut stream = el.to_html_stream_in_order();

        assert_eq!(stream.next().await.unwrap(), "<p>Before Suspense");
        assert_eq!(stream.next().await.unwrap(), "<!>Suspended</p>");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn in_order_nested_async_blocks() {
        let el: HtmlElement<_, _, _, Dom> = main().child((
            "Before Suspense",
            async {
                sleep(Duration::from_millis(250)).await;
                p().child((
                    "Before inner Suspense",
                    async {
                        sleep(Duration::from_millis(250)).await;
                        "Inner Suspense"
                    }
                    .suspend(),
                ))
            }
            .suspend(),
        ));
        let mut stream = el.to_html_stream_in_order();

        assert_eq!(stream.next().await.unwrap(), "<main>Before Suspense");
        assert_eq!(stream.next().await.unwrap(), "<p>Before inner Suspense");
        assert_eq!(
            stream.next().await.unwrap(),
            "<!>Inner Suspense</p></main>"
        );
    }

    #[tokio::test]
    async fn out_of_order_stream_of_sync_content_ready_immediately() {
        let el: HtmlElement<Main, _, _, Dom> = main().child(p().child((
            "Hello, ",
            em().child("beautiful"),
            " world!",
        )));
        let mut stream = el.to_html_stream_out_of_order();

        let html = stream.next().await.unwrap();
        assert_eq!(
            html,
            "<main><p>Hello, <em>beautiful</em> world!</p></main>"
        );
    }

    #[tokio::test]
    async fn out_of_order_single_async_block_in_stream() {
        let el = async {
            sleep(Duration::from_millis(250)).await;
            "Suspended"
        }
        .suspend()
        .with_fallback("Loading...");
        let mut stream =
            <Suspend<false, _, _> as RenderHtml<Dom>>::to_html_stream_out_of_order(
                el,
            );

        assert_eq!(
            stream.next().await.unwrap(),
            "<!--s-1-o-->Loading...<!--s-1-c-->"
        );
        assert_eq!(
            stream.next().await.unwrap(),
            "<template id=\"1-f\">Suspended</template><script>(function() { \
             let id = \"1-\";let open = undefined;let close = undefined;let \
             walker = document.createTreeWalker(document.body, \
             NodeFilter.SHOW_COMMENT);while(walker.nextNode()) \
             {if(walker.currentNode.textContent == `s-${id}o`){ \
             open=walker.currentNode; } else \
             if(walker.currentNode.textContent == `s-${id}c`) { close = \
             walker.currentNode;}}let range = new Range(); \
             range.setStartAfter(open); range.setEndBefore(close); \
             range.deleteContents(); let tpl = \
             document.getElementById(`${id}f`); \
             close.parentNode.insertBefore(tpl.content.cloneNode(true), \
             close);})()</script>"
        );
    }

    #[tokio::test]
    async fn out_of_order_inside_element_in_stream() {
        let el: HtmlElement<_, _, _, Dom> = p().child((
            "Before Suspense",
            async {
                sleep(Duration::from_millis(250)).await;
                "Suspended"
            }
            .suspend()
            .with_fallback("Loading..."),
            "After Suspense",
        ));
        let mut stream = el.to_html_stream_out_of_order();

        assert_eq!(
            stream.next().await.unwrap(),
            "<p>Before Suspense<!--s-1-o--><!>Loading...<!--s-1-c-->After \
             Suspense</p>"
        );
        assert!(stream.next().await.unwrap().contains("Suspended"));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn out_of_order_nested_async_blocks() {
        let el: HtmlElement<_, _, _, Dom> = main().child((
            "Before Suspense",
            async {
                sleep(Duration::from_millis(250)).await;
                p().child((
                    "Before inner Suspense",
                    async {
                        sleep(Duration::from_millis(250)).await;
                        "Inner Suspense"
                    }
                    .suspend()
                    .with_fallback("Loading Inner..."),
                    "After inner Suspense",
                ))
            }
            .suspend()
            .with_fallback("Loading..."),
            "After Suspense",
        ));
        let mut stream = el.to_html_stream_out_of_order();

        assert_eq!(
            stream.next().await.unwrap(),
            "<main>Before Suspense<!--s-1-o--><!>Loading...<!--s-1-c-->After \
             Suspense</main>"
        );
        let loading_inner = stream.next().await.unwrap();
        assert!(loading_inner.contains(
            "<p>Before inner Suspense<!--s-1-1-o--><!>Loading \
             Inner...<!--s-1-1-c-->After inner Suspense</p>"
        ));
        assert!(loading_inner.contains("let id = \"1-\";"));

        let inner = stream.next().await.unwrap();
        assert!(inner.contains("Inner Suspense"));
        assert!(inner.contains("let id = \"1-1-\";"));

        assert!(stream.next().await.is_none());
    }
}
*/
