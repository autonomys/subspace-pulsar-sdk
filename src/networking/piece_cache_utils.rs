use subspace_core_primitives::Piece;
use subspace_farmer::utils::piece_cache::PieceCache;
use subspace_networking::libp2p::kad::record::Key;

pub struct And<A, B> {
    first: A,
    second: B,
}

impl<A, B> And<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A, B> PieceCache for And<A, B>
where
    A: PieceCache,
    B: PieceCache,
{
    type KeysIterator = std::iter::Chain<
        <A::KeysIterator as std::iter::IntoIterator>::IntoIter,
        <B::KeysIterator as std::iter::IntoIterator>::IntoIter,
    >;

    fn should_cache(&self, key: &Key) -> bool {
        self.first.should_cache(key) || self.second.should_cache(key)
    }

    fn add_piece(&mut self, key: Key, piece: Piece) {
        if self.first.should_cache(&key) {
            self.first.add_piece(key.clone(), piece.clone());
        }
        if self.second.should_cache(&key) {
            self.second.add_piece(key, piece);
        }
    }

    fn get_piece(&self, key: &Key) -> Option<Piece> {
        if let result @ Some(_) = self.first.get_piece(key) {
            return result;
        }
        self.second.get_piece(key)
    }

    fn keys(&self) -> Self::KeysIterator {
        self.first.keys().into_iter().chain(self.second.keys())
    }
}
