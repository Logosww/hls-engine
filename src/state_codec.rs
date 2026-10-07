//! Private, versioned checkpoint encoding. No media payload or transport credentials.
//! Fixed-width big-endian numbers keep the archive independent of Rust/JS word sizes.
use std::collections::VecDeque;

pub(crate) type DecodeResult<T> = Result<T, ()>;
pub(crate) struct Reader<'a>(pub &'a [u8]);
impl<'a> Reader<'a> {
    pub fn take(&mut self, size: usize) -> DecodeResult<&'a [u8]> {
        if size > self.0.len() {
            return Err(());
        }
        let (value, rest) = self.0.split_at(size);
        self.0 = rest;
        Ok(value)
    }
}
pub(crate) trait StateCodec: Sized {
    fn put(&self, out: &mut Vec<u8>);
    fn get(input: &mut Reader<'_>) -> DecodeResult<Self>;
}
macro_rules! number {
    ($($t:ty),*) => { $(impl StateCodec for $t {
        fn put(&self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_be_bytes()); }
        fn get(input: &mut Reader<'_>) -> DecodeResult<Self> {
            Ok(Self::from_be_bytes(input.take(std::mem::size_of::<Self>())?.try_into().map_err(|_| ())?))
        }
    })* };
}
number!(u8, u16, u32, u64, i128);
impl StateCodec for usize {
    fn put(&self, out: &mut Vec<u8>) {
        (*self as u64).put(out);
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        u64::get(r)?.try_into().map_err(|_| ())
    }
}
impl StateCodec for bool {
    fn put(&self, out: &mut Vec<u8>) {
        u8::from(*self).put(out);
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        match u8::get(r)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(()),
        }
    }
}
impl<T: StateCodec> StateCodec for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        self.is_some().put(out);
        if let Some(v) = self {
            v.put(out);
        }
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        if bool::get(r)? {
            Ok(Some(T::get(r)?))
        } else {
            Ok(None)
        }
    }
}
impl<T: StateCodec> StateCodec for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        for v in self {
            v.put(out);
        }
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        let count = usize::get(r)?;
        // All encoded elements occupy at least one byte. Bound allocation before reserving.
        if count > r.0.len() || count > 1_048_576 {
            return Err(());
        }
        // Do not preallocate from an untrusted count. Decode an element before
        // growing, so a tiny corrupt archive cannot reserve a large Vec<T>.
        let mut values = Vec::new();
        for _ in 0..count {
            values.push(T::get(r)?);
        }
        Ok(values)
    }
}
impl<T: StateCodec> StateCodec for VecDeque<T> {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        for v in self {
            v.put(out);
        }
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        Ok(Vec::<T>::get(r)?.into())
    }
}
impl<T: StateCodec, const N: usize> StateCodec for [T; N] {
    fn put(&self, out: &mut Vec<u8>) {
        for v in self {
            v.put(out);
        }
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        (0..N)
            .map(|_| T::get(r))
            .collect::<DecodeResult<Vec<_>>>()?
            .try_into()
            .map_err(|_| ())
    }
}
impl StateCodec for String {
    fn put(&self, out: &mut Vec<u8>) {
        self.len().put(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        let len = usize::get(r)?;
        std::str::from_utf8(r.take(len)?)
            .map(str::to_owned)
            .map_err(|_| ())
    }
}
impl<A: StateCodec, B: StateCodec> StateCodec for (A, B) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        Ok((A::get(r)?, B::get(r)?))
    }
}
impl<A: StateCodec, B: StateCodec, C: StateCodec> StateCodec for (A, B, C) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
        self.2.put(out);
    }
    fn get(r: &mut Reader<'_>) -> DecodeResult<Self> {
        Ok((A::get(r)?, B::get(r)?, C::get(r)?))
    }
}
macro_rules! state_struct {
    ($name:ty {$($field:ident),* $(,)?}) => {
        impl $crate::state_codec::StateCodec for $name {
            fn put(&self, out: &mut Vec<u8>) { $( $crate::state_codec::StateCodec::put(&self.$field, out); )* }
            fn get(r: &mut $crate::state_codec::Reader<'_>) -> $crate::state_codec::DecodeResult<Self> {
                Ok(Self { $($field: $crate::state_codec::StateCodec::get(r)?),* })
            }
        }
    };
}
pub(crate) use state_struct;
macro_rules! state_enum {
    ($name:ty {$($tag:literal => $variant:ident),* $(,)?}) => {
        impl $crate::state_codec::StateCodec for $name {
            fn put(&self, out: &mut Vec<u8>) { match self { $(Self::$variant => out.push($tag)),* } }
            fn get(r: &mut $crate::state_codec::Reader<'_>) -> $crate::state_codec::DecodeResult<Self> {
                match r.take(1)?[0] { $($tag => Ok(Self::$variant)),*, _ => Err(()) }
            }
        }
    };
}
pub(crate) use state_enum;
