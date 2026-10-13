use super::*;

#[test]
fn up_and_down_walk_the_history() {
    let mut h = History::<3>::new();
    assert_eq!(h.older(b"x"), None, "nothing yet");
    h.push(b"ls /home");
    h.push(b"cat a");
    h.push(b"   ");
    h.push(b"cat a");
    assert_eq!(h.older(b"typing"), Some(&b"cat a"[..]));
    assert_eq!(h.older(b"ignored"), Some(&b"ls /home"[..]));
    assert_eq!(h.older(b"ignored"), None, "the oldest");
    assert_eq!(h.newer(), Some(&b"cat a"[..]));
    assert_eq!(h.newer(), Some(&b"typing"[..]), "back to the draft");
    assert_eq!(h.newer(), None);

    // Full: the oldest goes.
    h.push(b"one");
    h.push(b"two");
    assert_eq!(h.older(b""), Some(&b"two"[..]));
    assert_eq!(h.older(b""), Some(&b"one"[..]));
    assert_eq!(h.older(b""), Some(&b"cat a"[..]));
    assert_eq!(h.older(b""), None);
    // Entering a line starts the walk again.
    h.push(b"three");
    assert_eq!(h.older(b""), Some(&b"three"[..]));
}

#[test]
fn completing_words() {
    assert_eq!(last_word(b"run fe"), 4);
    assert_eq!(last_word(b"fe"), 0);
    assert_eq!(last_word(b"cat "), 4);

    let mut out = [0u8; MAX + 1];
    let mut m = Matches::new(b"fe");
    for name in ["fetch", "ls", "fs"] {
        m.offer(name.as_bytes(), false);
    }
    assert_eq!(m.count(), 1);
    let len = m.completion(&mut out);
    assert_eq!(&out[..len], b"tch ");

    // Several: what they share.
    let mut m = Matches::new(b"u");
    for name in ["update", "uptime", "uname", "user"] {
        m.offer(name.as_bytes(), false);
    }
    assert_eq!(m.count(), 4);
    assert_eq!(m.completion(&mut out), 0, "they part right away");
    let mut m = Matches::new(b"up");
    m.offer(b"update", false);
    m.offer(b"uptime", false);
    let len = m.completion(&mut out);
    assert_eq!((&out[..len], m.count()), (&b""[..], 2));
    let mut m = Matches::new(b"wr");
    m.offer(b"write", false);
    m.offer(b"write", false);
    assert_eq!(m.count(), 1, "the same name twice");

    // A directory ends with `/`.
    let mut m = Matches::new(b"ho");
    m.offer(b"home", true);
    let len = m.completion(&mut out);
    assert_eq!(&out[..len], b"me/");

    // Shared start longer than the prefix.
    let mut m = Matches::new(b"p");
    m.offer(b"picture.png", false);
    m.offer(b"picture.jpg", false);
    let len = m.completion(&mut out);
    assert_eq!(&out[..len], b"icture.");
    assert_eq!(Matches::new(b"zz").completion(&mut out), 0);
}
