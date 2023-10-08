use std::sync::Arc;

use atomig::{Atom, Atomic, Ordering};
use dashmap::{DashMap, DashSet};
use itertools::{izip, Itertools};
use serde::{Deserialize, Serialize};

use crate::game_manager::{
    game::{self, Game, IncomingHostMessage, IncomingMessage, IncomingPlayerMessage},
    session::Tunnel,
    watcher::{WatcherId, WatcherValueKind},
};

use super::config::FuizConfig;

#[derive(Atom, Clone, Copy, Debug, Default)]
#[repr(u8)]
enum SlideState {
    #[default]
    Unstarted,
    List,
    Leaderboard,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Slide {
    points_awarded: u64,
    answers: Vec<String>,
    board_size: usize,

    #[serde(skip)]
    user_votes: DashMap<usize, DashSet<WatcherId>>,
    #[serde(skip)]
    crossed: DashSet<usize>,
    #[serde(skip)]
    slide_state: Arc<Atomic<SlideState>>,
}

#[serde_with::serde_as]
#[derive(Debug, Serialize, Clone)]
pub enum OutgoingMessage {
    List {
        index: usize,
        count: usize,
        all_statements: Vec<Word>,
        assigned_statement: Vec<usize>,
        crossed: Vec<usize>,
        user_votes: Vec<usize>,
    },
    Cross {
        crossed: Vec<usize>,
    },
    Votes {
        user_votes: Vec<usize>,
    },
    Leaderboard {
        winners: Vec<String>,
    },
}

#[derive(Debug, Serialize, Clone)]
pub struct Word {
    id: usize,
    text: String,
}

impl game::OutgoingMessage for OutgoingMessage {
    fn identifier(&self) -> &'static str {
        "Bingo"
    }
}

#[serde_with::serde_as]
#[derive(Debug, Serialize, Clone)]
pub enum StateMessage {
    List {
        index: usize,
        count: usize,
        all_statements: Vec<Word>,
        assigned_statement: Vec<usize>,
        crossed: Vec<usize>,
        user_votes: Vec<usize>,
    },
    Leaderboard {
        index: usize,
        count: usize,
        winners: Vec<String>,
    },
}

impl game::StateMessage for StateMessage {
    fn identifier(&self) -> &'static str {
        "Bingo"
    }
}

fn is_bingo(cells: &[bool]) -> bool {
    let col_count = (cells.len() as f64).sqrt().ceil() as usize;

    let row_count = cells.len() / col_count;

    // check rows
    for row in cells.chunks_exact(col_count) {
        if row.iter().all(|x| *x) {
            return true;
        }
    }

    // check cols
    for col in izip!(cells.chunks(col_count)).filter(|x| x.len() == row_count) {
        if col.iter().all(|x| *x) {
            return true;
        }
    }

    // check diagonals
    if col_count == row_count {
        // upper left to bottom right diagonal
        if col_count * row_count == cells.len()
            && (0..col_count).all(|i| cells.get(i * col_count + i) == Some(&true))
        {
            return true;
        }

        // bottom left to up right diagonal
        if (0..col_count).all(|i| cells.get((row_count - i - 1) * col_count + i) == Some(&true)) {
            return true;
        }
    }

    false
}

impl Slide {
    pub async fn play<T: Tunnel>(
        &self,
        game: &Game<T>,
        _fuiz: &FuizConfig,
        index: usize,
        count: usize,
    ) {
        self.send_list(game, index, count).await;
    }

    async fn send_list<T: Tunnel>(&self, game: &Game<T>, index: usize, count: usize) {
        if self.change_state(SlideState::Unstarted, SlideState::List) {
            game.announce_with(|w, v| {
                Some(OutgoingMessage::List {
                    index,
                    count,
                    all_statements: self
                        .answers
                        .iter()
                        .enumerate()
                        .map(|(id, s)| Word {
                            id,
                            text: s.to_owned(),
                        })
                        .collect_vec(),
                    assigned_statement: match v {
                        WatcherValueKind::Host => Vec::new(),
                        WatcherValueKind::Unassigned => Vec::new(),
                        WatcherValueKind::Player => {
                            let mut rng = fastrand::Rng::new();
                            rng.seed(w.get_seed());
                            rng.choose_multiple(0..self.answers.len(), self.board_size)
                        }
                    },
                    crossed: self.crossed.iter().map(|x| *x).collect_vec(),
                    user_votes: self.get_user_votes(),
                })
            })
            .await;
        }
    }

    fn change_state(&self, before: SlideState, after: SlideState) -> bool {
        self.slide_state
            .compare_exchange(before, after, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn state(&self) -> SlideState {
        self.slide_state.load(Ordering::SeqCst)
    }

    fn get_winners<T: Tunnel>(&self, game: &Game<T>) -> Vec<String> {
        self.get_winners_id(game)
            .into_iter()
            .flat_map(|x| game.get_name(x))
            .collect_vec()
    }

    fn get_winners_id<T: Tunnel>(&self, game: &Game<T>) -> Vec<WatcherId> {
        game.players()
            .iter()
            .filter_map(|w| {
                let mut rng = fastrand::Rng::new();
                rng.seed(w.get_seed());
                let players_board = rng.choose_multiple(0..self.answers.len(), self.board_size);

                let bingo_board = players_board
                    .iter()
                    .map(|x| self.crossed.contains(&x))
                    .collect_vec();

                if is_bingo(&bingo_board) {
                    Some(*w)
                } else {
                    None
                }
            })
            .collect_vec()
    }

    async fn send_leaderboard<T: Tunnel>(&self, game: &Game<T>) {
        if self.change_state(SlideState::List, SlideState::Leaderboard) {
            for id in self.get_winners_id(game) {
                game.leaderboard.add_score(id, self.points_awarded);
            }
            game.announce(OutgoingMessage::Leaderboard {
                winners: self.get_winners(game),
            })
            .await;
        }
    }

    pub fn state_message<T: Tunnel>(
        &self,
        watcher_id: WatcherId,
        watcher_kind: WatcherValueKind,
        game: &Game<T>,
        index: usize,
        count: usize,
    ) -> StateMessage {
        match self.state() {
            SlideState::Unstarted | SlideState::List => StateMessage::List {
                index,
                count,
                all_statements: self
                    .answers
                    .iter()
                    .enumerate()
                    .map(|(id, s)| Word {
                        id,
                        text: s.to_owned(),
                    })
                    .collect_vec(),
                assigned_statement: match watcher_kind {
                    WatcherValueKind::Host => Vec::new(),
                    WatcherValueKind::Unassigned => Vec::new(),
                    WatcherValueKind::Player => {
                        let mut rng = fastrand::Rng::new();
                        rng.seed(watcher_id.get_seed());
                        rng.choose_multiple(0..self.answers.len(), self.board_size)
                    }
                },
                crossed: self.crossed.iter().map(|x| *x).collect_vec(),
                user_votes: self.get_user_votes(),
            },
            SlideState::Leaderboard => StateMessage::Leaderboard {
                index,
                count,
                winners: self.get_winners(game),
            },
        }
    }

    fn get_user_votes(&self) -> Vec<usize> {
        let mut user_votes = vec![0; self.answers.len()];
        for (i, users) in self.user_votes.clone().into_read_only().iter() {
            if let Some(u) = user_votes.get_mut(*i) {
                *u = users.len();
            }
        }
        user_votes
    }

    pub async fn receive_message<T: Tunnel>(
        &self,
        game: &Game<T>,
        fuiz: &FuizConfig,
        watcher_id: WatcherId,
        message: IncomingMessage,
        index: usize,
        count: usize,
    ) {
        match message {
            IncomingMessage::Host(host_message) => match host_message {
                IncomingHostMessage::Next => match self.slide_state.load(Ordering::SeqCst) {
                    SlideState::Unstarted => self.send_list(game, index, count).await,
                    SlideState::List => self.send_leaderboard(game).await,
                    SlideState::Leaderboard => fuiz.play_slide(game, index + 1).await,
                },
                IncomingHostMessage::Index(u) => {
                    self.crossed.insert(u);
                    let winners = self.get_winners(game);
                    game.announce(OutgoingMessage::Cross {
                        crossed: self.crossed.iter().map(|x| *x).collect_vec(),
                    })
                    .await;
                    if !winners.is_empty() {
                        self.send_leaderboard(game).await;
                    }
                }
            },
            IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(v)) => {
                {
                    let s = self.user_votes.entry(v).or_default();
                    s.insert(watcher_id);
                }
                game.announce(OutgoingMessage::Votes {
                    user_votes: self.get_user_votes(),
                })
                .await;
                if !self.get_winners_id(game).is_empty() {
                    self.send_leaderboard(game).await;
                }
            }
            _ => (),
        }
    }
}