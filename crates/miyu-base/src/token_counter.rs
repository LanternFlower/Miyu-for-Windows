use anyhow::{ensure, Result};
use fancy_regex::Regex;
use std::cell::OnceCell;
use std::collections::BinaryHeap;
use std::hash::Hasher;
use std::sync::OnceLock;

type Rank = u32;

/// o200k 词表的紧凑查找表。
///
/// 通用 HashMap 每条要 24B（&[u8] 胖指针 + rank + 对齐），20 万词 ≈ 6.4MB
/// 常驻堆。这里键本来就全部指向同一个嵌入 blob，(offset,len) 8B 就够：
/// 开放寻址 + 每槽 1 字节 hash tag 先行过滤 + 命中前逐字节校验，
/// 12B/槽 + tag ≈ 3.4MB，语义与 HashMap 完全一致（无哈希碰撞风险，
/// 因为最终判定靠字节比较）。词表构建后只读。
struct RankTable {
    data: &'static [u8],
    slots: Vec<Slot>,
    tags: Vec<u8>,
    mask: usize,
}

#[derive(Clone, Copy)]
struct Slot {
    offset: u32,
    len: u16,
    rank: Rank,
}

const EMPTY_RANK: Rank = Rank::MAX;

fn piece_hash(piece: &[u8]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    hasher.write(piece);
    hasher.finish()
}

impl RankTable {
    fn with_capacity(data: &'static [u8], entries: usize) -> Self {
        // 载荷 ~76%：探测串平均个位数,tag 过滤后每次探测只是一次字节比较
        // 的机会成本;比这更稀就把省下的内存还回去了。
        let slots = (entries * 4 / 3).next_power_of_two();
        Self {
            data,
            slots: vec![
                Slot {
                    offset: 0,
                    len: 0,
                    rank: EMPTY_RANK,
                };
                slots
            ],
            tags: vec![0; slots],
            mask: slots - 1,
        }
    }

    fn insert(&mut self, offset: usize, len: usize, rank: Rank) -> bool {
        let piece = &self.data[offset..offset + len];
        let hash = piece_hash(piece);
        let tag = (hash >> 56) as u8 | 1;
        let mut index = hash as usize & self.mask;
        loop {
            let slot = self.slots[index];
            if slot.rank == EMPTY_RANK {
                self.slots[index] = Slot {
                    offset: offset as u32,
                    len: len as u16,
                    rank,
                };
                self.tags[index] = tag;
                return true;
            }
            if self.tags[index] == tag
                && slot.len as usize == len
                && &self.data[slot.offset as usize..slot.offset as usize + len] == piece
            {
                return false; // 重复词条
            }
            index = (index + 1) & self.mask;
        }
    }

    #[inline]
    fn get(&self, piece: &[u8]) -> Option<Rank> {
        let hash = piece_hash(piece);
        let tag = (hash >> 56) as u8 | 1;
        let mut index = hash as usize & self.mask;
        loop {
            let slot = self.slots[index];
            if slot.rank == EMPTY_RANK {
                return None;
            }
            if self.tags[index] == tag
                && slot.len as usize == piece.len()
                && &self.data[slot.offset as usize..slot.offset as usize + slot.len as usize]
                    == piece
            {
                return Some(slot.rank);
            }
            index = (index + 1) & self.mask;
        }
    }

    #[inline]
    fn contains(&self, piece: &[u8]) -> bool {
        self.get(piece).is_some()
    }
}
const O200K_PATTERN: &str = concat!(
    r#"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?"#,
    "|",
    r#"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?"#,
    "|",
    r#"\p{N}{1,3}"#,
    "|",
    r#" ?[^\s\p{L}\p{N}]+[\r\n/]*"#,
    "|",
    r#"\s*[\r\n]+"#,
    "|",
    r#"\s+(?!\S)"#,
    "|",
    r#"\s+"#
);

static COUNTER: OnceLock<CoreBpeCounter> = OnceLock::new();

pub fn count(text: &str) -> usize {
    COUNTER
        .get_or_init(|| CoreBpeCounter::new().expect("embedded o200k vocabulary must be valid"))
        .count_ordinary(text)
}

struct CoreBpeCounter {
    encoder: RankTable,
    regex: Regex,
}

thread_local! {
    // 每个真正计数的线程惰性克隆一份正则（编译产物 + 独立 cache 池），
    // 零争用。进程里只有 COUNTER 这一个实例，TLS 不会串源。
    static REGEX_TLS: OnceCell<Regex> = const { OnceCell::new() };
}

impl CoreBpeCounter {
    fn new() -> Result<Self> {
        let data = include_bytes!(concat!(env!("OUT_DIR"), "/o200k_base.bin"));
        let mut encoder = RankTable::with_capacity(data, 199_998);
        let mut cursor = 0usize;
        let mut rank = 0u32;
        while cursor < data.len() {
            ensure!(cursor + 2 <= data.len(), "truncated o200k token length");
            let len = u16::from_le_bytes([data[cursor], data[cursor + 1]]) as usize;
            cursor += 2;
            let end = cursor + len;
            ensure!(end <= data.len(), "truncated o200k token payload");
            ensure!(encoder.insert(cursor, len, rank), "duplicate o200k token");
            cursor = end;
            rank += 1;
        }
        ensure!(rank == 199_998, "unexpected o200k vocabulary size");

        let regex = Regex::new(O200K_PATTERN)?;
        Ok(Self { encoder, regex })
    }

    fn count_ordinary(&self, text: &str) -> usize {
        REGEX_TLS.with(|cell| {
            let regex = cell.get_or_init(|| self.regex.clone());
            regex
                .find_iter(text)
                .map(|mat| {
                    let piece = mat.unwrap().as_str().as_bytes();
                    if self.encoder.contains(piece) {
                        1
                    } else {
                        byte_pair_count(piece, &self.encoder)
                    }
                })
                .sum()
        })
    }
}

fn rank(ranks: &RankTable, piece: &[u8]) -> Rank {
    ranks.get(piece).unwrap_or(Rank::MAX)
}

fn byte_pair_count(piece: &[u8], ranks: &RankTable) -> usize {
    if piece.len() == 1 {
        return 1;
    }
    if piece.len() < 100 {
        return byte_pair_merge(ranks, piece).len() - 1;
    }
    byte_pair_merge_large(ranks, piece).len()
}

fn byte_pair_merge(ranks: &RankTable, piece: &[u8]) -> Vec<(usize, Rank)> {
    let mut parts = Vec::with_capacity(piece.len() + 1);
    let mut min_rank = (Rank::MAX, usize::MAX);
    for i in 0..piece.len() - 1 {
        let rank = rank(ranks, &piece[i..i + 2]);
        if rank < min_rank.0 {
            min_rank = (rank, i);
        }
        parts.push((i, rank));
    }
    parts.push((piece.len() - 1, Rank::MAX));
    parts.push((piece.len(), Rank::MAX));

    let get_rank = |parts: &Vec<(usize, Rank)>, i: usize| {
        if i + 3 < parts.len() {
            rank(ranks, &piece[parts[i].0..parts[i + 3].0])
        } else {
            Rank::MAX
        }
    };
    while min_rank.0 != Rank::MAX {
        let i = min_rank.1;
        if i > 0 {
            parts[i - 1].1 = get_rank(&parts, i - 1);
        }
        parts[i].1 = get_rank(&parts, i);
        parts.remove(i + 1);

        min_rank = (Rank::MAX, usize::MAX);
        for (i, &(_, rank)) in parts[..parts.len() - 1].iter().enumerate() {
            if rank < min_rank.0 {
                min_rank = (rank, i);
            }
        }
    }
    parts
}

#[derive(Eq, PartialEq, Clone, Copy)]
struct Merge {
    start: usize,
    rank: Rank,
}

impl Ord for Merge {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .rank
            .cmp(&self.rank)
            .then_with(|| other.start.cmp(&self.start))
    }
}

impl PartialOrd for Merge {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct State {
    prev: usize,
    end: usize,
    next_end: usize,
    next_rank: Rank,
    cur_rank: Rank,
}

fn byte_pair_merge_large(ranks: &RankTable, piece: &[u8]) -> Vec<Rank> {
    let mut state = Vec::with_capacity(piece.len());
    state.push(State {
        prev: usize::MAX,
        end: 1,
        next_end: 2,
        next_rank: Rank::MAX,
        cur_rank: Rank::MAX,
    });
    let mut heap = BinaryHeap::with_capacity(piece.len());
    for i in 0..piece.len() - 1 {
        let pair_rank = rank(ranks, &piece[i..i + 2]);
        if pair_rank != Rank::MAX {
            heap.push(Merge {
                start: i,
                rank: pair_rank,
            });
            state[i].next_rank = pair_rank;
        }
        state.push(State {
            prev: i,
            end: i + 2,
            next_end: i + 3,
            next_rank: Rank::MAX,
            cur_rank: Rank::MAX,
        });
    }

    let potential_merge =
        |state: &mut Vec<State>, heap: &mut BinaryHeap<Merge>, start: usize, next_end: usize| {
            state[start].next_end = next_end;
            state[start].next_rank = Rank::MAX;
            if next_end <= piece.len() {
                let next_rank = rank(ranks, &piece[start..next_end]);
                if next_rank != Rank::MAX {
                    heap.push(Merge {
                        start,
                        rank: next_rank,
                    });
                    state[start].next_rank = next_rank;
                }
            }
        };

    while let Some(left) = heap.pop() {
        if left.rank == Rank::MAX {
            break;
        }
        if left.rank != state[left.start].next_rank {
            continue;
        }
        let left_start = left.start;
        let right_start = state[left_start].end;
        let right_end = state[left_start].next_end;
        let right_next_end = state[right_start].next_end;
        state[left_start].cur_rank = state[left_start].next_rank;
        state[left_start].end = right_end;
        potential_merge(&mut state, &mut heap, left_start, right_next_end);
        if right_end < state.len() {
            state[right_end].prev = left_start;
        }
        if left_start > 0 {
            let prev_start = state[left_start].prev;
            potential_merge(&mut state, &mut heap, prev_start, right_end);
        }
        state[right_start].next_rank = Rank::MAX;
    }

    let mut result = Vec::new();
    let mut i = 0;
    while i < state.len() {
        result.push(if state[i].cur_rank != Rank::MAX {
            state[i].cur_rank
        } else {
            rank(ranks, &piece[i..state[i].end])
        });
        i = state[i].end;
    }
    result
}
