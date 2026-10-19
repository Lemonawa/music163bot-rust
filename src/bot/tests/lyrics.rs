use super::*;
use crate::music_api::{Album, Artist, SearchSong, SongDetail};

fn artist(name: &str) -> Artist {
    Artist {
        id: 1,
        name: name.to_string(),
    }
}

fn album() -> Album {
    Album {
        id: 1,
        name: String::new(),
        pic_url: None,
    }
}

#[test]
fn lyric_filename_uses_full_search_title_when_detail_title_is_truncated() {
    let search_song = SearchSong {
        id: 1,
        name: "$・¥・€".to_string(),
        artists: vec![artist("鈴木愛奈")],
        album: album(),
        duration: 0,
    };
    let song_detail = SongDetail {
        id: 1,
        name: "$".to_string(),
        dt: None,
        ar: Some(vec![artist("鈴木愛奈")]),
        al: None,
    };

    assert_eq!(
        build_lyric_filename(Some(&search_song), &song_detail),
        "鈴木愛奈 - $・¥・€.lrc"
    );
}

#[test]
fn lyric_filename_falls_back_to_song_detail_for_direct_id_lookups() {
    let song_detail = SongDetail {
        id: 1,
        name: "Example Song".to_string(),
        dt: None,
        ar: Some(vec![artist("Example Artist")]),
        al: None,
    };

    assert_eq!(
        build_lyric_filename(None, &song_detail),
        "Example Artist - Example Song.lrc"
    );
}
