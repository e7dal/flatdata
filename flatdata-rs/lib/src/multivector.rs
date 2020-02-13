use crate::{
    error::ResourceStorageError,
    memory,
    multiarrayview::MultiArrayView,
    storage::ResourceHandle,
    structs::{IndexStruct, VariadicRefFactory, VariadicStruct},
    vector::ExternalVector,
};

use std::{borrow::BorrowMut, fmt, io, marker};

/// A container for writing an indexed sequence of heterogeneous data items.
///
/// The concept of a multivector is used for storing and reading heterogeneous
/// flatdata structs in/from the same container. The data is indexed by
/// integers. Each index refers to a bucket which may contain a variable number
/// of items of different types unified in the same variant enum `Ts`.
/// Such bucket may also be empty, which allows to represent sparse data in a
/// multivector. For those who are familiar with C++'s `std::multimap` data
/// structure, a multivector can be considered as a `std::multimap` mapping
/// integers to sequences of variable length.
///
/// A `MultiVector` corresponds rather to [`ExternalVector`] than to
/// [`Vector`], in the sense that the items are flushed to storage whenever the
/// internal buffer is full. In particular, it is only possible to modify the
/// last bucket. There is no access to the buckets previously stored.
///
/// For accessing and reading the data stored by in multivector, cf.
/// [`MultiArrayView`].
///
/// A multivector *must* be closed, after the last element was written to it.
/// After closing, it can not be used anymore.
///
/// Internally data is stored like this:
///
/// * `Index`: `Vector<Idx>` - encodes start/end byte in `Data` array for each
/// element `i`. * `Data`: `Vec<u8>` - sequence of serialized (`Tag`,
/// `ItemData`) tuples, where `Tag` encodes the the variant type, and
/// `ItemData` contains the underlying variant data. `Tag` has size of 1 byte,
/// `ItemData` is of size `Ts::Type::SIZE_IN_BYTES`.
///
/// # Examples
/// ``` flatdata
/// struct A {
///     x : u32 : 16;
///     y : u32 : 16;
/// }
///
/// struct B {
///     id : u32 : 16;
/// }
///
/// archive X {
///    ab : multivector< 16, A, B >;
/// }
/// ```
///
/// ```
/// # #[macro_use] extern crate flatdata;
/// # fn main() {
/// # use flatdata::{
/// #     ArchiveBuilder, Archive, MemoryResourceStorage,
/// # };
/// #
/// # // define structs usually generated by flatdata's generator
/// #
/// # define_struct!(
/// #     A,
/// #     RefA,
/// #     RefMutA,
/// #     "Schema for A",
/// #     4,
/// #     (x, set_x, u32, u32, 0, 16),
/// #     (y, set_y, u32, u32, 16, 16));
/// #
/// #
/// # define_struct!(
/// #     B,
/// #     RefB,
/// #     RefMutB,
/// #     "Schema for B",
/// #     2,
/// #     (id, set_id, u32, u32, 0, 16));
/// #
/// # define_variadic_struct!(Ab, RefAb, BuilderAb,
/// #     IndexType16,
/// #     0 => ( A, A, add_a),
/// #     1 => ( B, B, add_b));
/// #
/// # define_index!(
/// #     IndexType16,
/// #     RefIndexType16,
/// #     RefMutIndexType16,
/// #     "Schema for INDEX_TYPE16",
/// #     2,
/// #     16
/// # );
/// #
/// # define_archive!(X, XBuilder, "Schema for X";
/// #     multivector(ab, false, "Schema for AB", start_ab, Ab, ab_index, IndexType16),
/// # );
/// #
/// // create multivector and serialize some data
/// let mut storage = MemoryResourceStorage::new("/root/multivec");
/// let mut builder = XBuilder::new(storage.clone()).expect("Fail to create builder");
/// let mut mv = builder.start_ab().expect("failed to create MultiVector");
/// let mut item = mv.grow().expect("grow failed");
/// let mut a = item.add_a();
/// a.set_x(1);
/// a.set_y(2);
///
/// let mut b = item.add_b();
/// b.set_id(42);
/// mv.close().expect("close failed");
///
/// // open multivector and read the data
/// let archive = X::open(storage).expect("open failed");
/// let mv = archive.ab();
///
/// assert_eq!(mv.len(), 1);
/// let mut item = mv.at(0);
/// match item.next().unwrap() {
///     RefAb::A(a) => assert_eq!((a.x(), a.y()), (1, 2)),
///     _ => assert!(false),
/// }
/// match item.next().unwrap() {
///     RefAb::B(b) => assert_eq!(b.id(), 42),
///     _ => assert!(false),
/// }
///
/// # }
/// ```
///
/// [`ExternalVector`]: struct.ExternalVector.html
/// [`Vector`]: struct.Vector.html
/// [`MultiArrayView`]: struct.MultiArrayView.html
pub struct MultiVector<'a, Ts>
where
    Ts: VariadicRefFactory,
{
    index: ExternalVector<'a, <Ts as VariadicStruct<'a>>::Index>,
    data: Vec<u8>,
    data_handle: ResourceHandle<'a>,
    size_flushed: usize,
    _phantom: marker::PhantomData<Ts>,
}

impl<'a, Ts> MultiVector<'a, Ts>
where
    Ts: VariadicRefFactory,
{
    /// Creates an empty multivector.
    pub fn new(
        index: ExternalVector<'a, <Ts as VariadicStruct<'a>>::Index>,
        data_handle: ResourceHandle<'a>,
    ) -> Self {
        Self {
            index,
            data: vec![0; memory::PADDING_SIZE],
            data_handle,
            size_flushed: 0,
            _phantom: marker::PhantomData,
        }
    }

    /// Appends a new item to the end of this multivector and returns a builder
    /// for it.
    ///
    /// The builder is used for storing different variants of `Ts` in the newly
    /// created item.
    ///
    /// Calling this method may flush data to storage (cf. [`flush`]), which
    /// may fail due to different IO reasons.
    ///
    /// [`flush`]: #method.flush
    pub fn grow(&mut self) -> io::Result<<Ts as VariadicStruct>::ItemMut> {
        if self.data.len() > 1024 * 1024 * 32 {
            self.flush()?;
        }
        self.add_to_index()?;
        Ok(<Ts as VariadicStruct>::create_mut(&mut self.data))
    }

    /// Flushes the not yet flushed content in this multivector to storage.
    ///
    /// Only data is flushed.
    fn flush(&mut self) -> io::Result<()> {
        self.data_handle
            .borrow_mut()
            .write(&self.data[..self.data.len() - memory::PADDING_SIZE])?;
        self.size_flushed += self.data.len() - memory::PADDING_SIZE;
        self.data.clear();
        self.data.resize(memory::PADDING_SIZE, 0);
        Ok(())
    }

    fn add_to_index(&mut self) -> io::Result<()> {
        let idx_mut = self.index.grow()?;
        <<Ts as VariadicStruct<'a>>::Index as IndexStruct>::set_index(
            idx_mut,
            self.size_flushed + self.data.len() - memory::PADDING_SIZE,
        );
        Ok(())
    }

    /// Flushes the remaining not yet flushed elements in this multivector and
    /// finalizes the data inside the storage.
    ///
    /// A multivector *must* be closed
    pub fn close(mut self) -> Result<MultiArrayView<'a, Ts>, ResourceStorageError> {
        let name: String = self.data_handle.name().into();
        let into_storage_error = |e| ResourceStorageError::from_io_error(e, name.clone());
        self.add_to_index().map_err(into_storage_error)?; // sentinel for last item
        self.flush().map_err(into_storage_error)?;
        let index_view = self.index.close()?;
        let data = self.data_handle.close()?;
        Ok(MultiArrayView::new(index_view, data))
    }
}

impl<'a, Ts> fmt::Debug for MultiVector<'a, Ts>
where
    Ts: VariadicRefFactory,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "MultiVector {{ len: {} }}", self.index.len())
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use crate::{
        arrayview::ArrayView,
        memstorage::MemoryResourceStorage,
        multiarrayview::MultiArrayView,
        storage::{create_multi_vector, ResourceStorage},
    };

    define_index!(Idx, RefIdx, RefMutIdx, "some_idx_schema", 4, 32);

    define_struct!(
        A,
        RefA,
        RefMutA,
        "no_schema",
        4,
        (x, set_x, u32, u32, 0, 16),
        (y, set_y, u32, u32, 16, 16)
    );

    define_variadic_struct!(Variant, RefVariant, BuilderVariant, Idx,
        0 => (A, A, add_a) );

    #[test]
    fn test_multi_vector() {
        let storage = MemoryResourceStorage::new("/root/resources");
        {
            let mut mv = create_multi_vector::<Variant>(&*storage, "multivector", "Some schema")
                .expect("failed to create MultiVector");
            {
                let mut item = mv.grow().expect("grow failed");
                {
                    let mut a = item.add_a();
                    a.set_x(1);
                    a.set_y(2);
                    assert_eq!(a.x(), 1);
                    assert_eq!(a.y(), 2);
                }
                {
                    let mut b = item.add_a();
                    b.set_x(3);
                    b.set_y(4);
                    assert_eq!(b.x(), 3);
                    assert_eq!(b.y(), 4);
                }
            }
            let view = mv.close().expect("close failed");

            // view can also be used directly after closing
            assert_eq!(view.len(), 1);
            let mut item = view.at(0);
            let a = item.next().unwrap();
            match a {
                RefVariant::A(ref a) => {
                    assert_eq!(a.x(), 1);
                    assert_eq!(a.y(), 2);
                }
            }
        }

        let index_resource = storage
            .read_and_check_schema("multivector_index", "index(Some schema)")
            .expect("read_and_check_schema failed");
        let index: ArrayView<Idx> = ArrayView::new(&index_resource);
        let resource = storage
            .read_and_check_schema("multivector", "Some schema")
            .expect("read_and_check_schema failed");
        let mv: MultiArrayView<Variant> = MultiArrayView::new(index, &resource);

        assert_eq!(mv.len(), 1);
        let mut item = mv.at(0);
        let a = item.next().unwrap();
        match a {
            RefVariant::A(ref a) => {
                assert_eq!(a.x(), 1);
                assert_eq!(a.y(), 2);
            }
        }
        let b = item.next().unwrap();
        match b {
            RefVariant::A(ref a) => {
                assert_eq!(a.x(), 3);
                assert_eq!(a.y(), 4);
            }
        }

        let x = {
            // test clone and lifetime of returned reference
            let mv_copy = mv.clone();
            mv_copy.at(0).next().unwrap()
        };
        match x {
            RefVariant::A(ref a) => {
                assert_eq!(a.x(), 1);
                assert_eq!(a.y(), 2);
            }
        }

        let x = {
            // test clone and lifetime of returned reference
            let mv_copy = mv.clone();
            mv_copy.iter().next().unwrap().next().unwrap()
        };
        match x {
            RefVariant::A(ref a) => {
                assert_eq!(a.x(), 1);
                assert_eq!(a.y(), 2);
            }
        }
    }
}
