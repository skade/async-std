use crate::io;
use crate::path::Path;
use crate::task::blocking;

/// Creates a hard link on the filesystem.
///
/// The `dst` path will be a link pointing to the `src` path. Note that operating systems often
/// require these two paths to be located on the same filesystem.
///
/// This function is an async version of [`std::fs::hard_link`].
///
/// [`std::fs::hard_link`]: https://doc.rust-lang.org/std/fs/fn.hard_link.html
///
/// # Errors
///
/// An error will be returned in the following situations:
///
/// * `src` does not point to an existing file.
/// * Some other I/O error occurred.
///
/// # Examples
///
/// ```no_run
/// # fn main() -> std::io::Result<()> { async_std::task::block_on(async {
/// #
/// use async_std::fs;
///
/// fs::hard_link("a.txt", "b.txt").await?;
/// #
/// # Ok(()) }) }
/// ```
pub async fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    let from = from.as_ref().to_owned();
    let to = to.as_ref().to_owned();
    blocking::spawn(move || std::fs::hard_link(&from, &to)).await
}
