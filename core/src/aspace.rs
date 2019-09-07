use log::{debug};

use super::arch::RVA;

const PAGE_SIZE: usize = 0x1000;


#[derive(Debug)]
pub enum Error {
    NotMapped,
}

fn page(rva: RVA) -> usize {
    // depends on PAGE_SIZE
    let v: u64 = rva.into();
    // #yolo
    (v as usize) >> 12
}

fn page_offset(rva: RVA) -> usize {
    // depends on PAGE_SIZE
    let v: u64 = rva.into();
    // #yolo
    (v as usize) & 0xFFF
}

pub struct Page<T: Default + Copy> {
    pub elements: [T; PAGE_SIZE],
}

impl<T: Default + Copy> Page<T> {
    pub fn new(items: &[T]) -> Page<T> {
        let mut page: Page<T> = Default::default();
        page.elements.copy_from_slice(items);
        page
    }
}

impl<T: Default + Copy> Default for Page<T> {
    fn default() -> Self {
        Page {
            elements: [Default::default(); PAGE_SIZE]
        }
    }
}

pub struct DenseAddressSpace<T: Default + Copy> {
    pages: Vec<Option<Page<T>>>
}

impl<T: Default + Copy> DenseAddressSpace<T> {
    pub fn with_capacity(capacity: RVA) -> DenseAddressSpace<T>{
        let page_count = page(capacity) + 1;
        let mut pages = Vec::with_capacity(page_count);
        pages.resize_with(page_count, || None);

        DenseAddressSpace {
            pages
        }
    }

    /// error if rva is not in a valid page.
    /// panic due to:
    ///   - rva must be page aligned.
    ///   - must be PAGE_SIZE number of items.
    fn map_page(&mut self, rva: RVA, items: &[T]) -> Result<(), Error> {
        if page_offset(rva) != 0 {
            panic!("invalid map address");
        }
        if items.len() != PAGE_SIZE {
            panic!("invalid map buffer size");
        }
        debug!("mapping page: {}", rva);
        if page(rva) > self.pages.len() {
            return Err(Error::NotMapped);
        }

        self.pages[page(rva)] = Some(Page::new(items));

        Ok(())
    }

    /// map the given items at the given address.
    ///
    /// error if rva or items are not in a valid page.
    /// panic due to:
    ///   - rva must be page aligned.
    ///   - must be multiple of PAGE_SIZE number of items.
    ///
    /// see example under `get`.
    pub fn map(&mut self, rva: RVA, items: &[T]) -> Result<(), Error> {
        for (i, chunk) in items.chunks_exact(PAGE_SIZE).enumerate() {
            self.map_page(rva + i * PAGE_SIZE, chunk)?;
        }
        Ok(())
    }

    /// map the default value (probably zero) at the given address for the given size.
    ///
    /// same error conditions as `map`.
    /// see example under `probe`.
    pub fn map_empty(&mut self, rva: RVA, size: usize) -> Result<(), Error> {
        self.map(rva, &vec![Default::default(); size])
    }

    /// is the given address mapped?
    ///
    /// ```
    /// use lancelot::arch::RVA;
    /// use lancelot::aspace::DenseAddressSpace;
    ///
    /// let mut d: DenseAddressSpace<u32> = DenseAddressSpace::with_capacity(0x2000.into());
    /// assert_eq!(d.probe(0x0.into()), false);
    /// assert_eq!(d.probe(0x1000.into()), false);
    ///
    /// d.map_empty(0x0.into(), 0x1000).expect("failed to map");
    /// assert_eq!(d.probe(0x0.into()), true);
    /// assert_eq!(d.probe(0x1000.into()), false);
    /// ```
    pub fn probe(&self, rva: RVA) -> bool {
        if page(rva) > self.pages.len() {
            return false;
        }

        return self.pages[page(rva)].is_some()
    }

    /// fetch one item from the given address.
    /// if the address is not mapped, then the result is `None`.
    ///
    /// ```
    /// use lancelot::arch::RVA;
    /// use lancelot::aspace::DenseAddressSpace;
    ///
    /// let mut d: DenseAddressSpace<u32> = DenseAddressSpace::with_capacity(0x2000.into());
    /// assert_eq!(d.get(0x0.into()), None);
    /// assert_eq!(d.get(0x1000.into()), None);
    ///
    /// d.map(0x1000.into(), &[0x1; 0x1000]).expect("failed to map");
    /// assert_eq!(d.get(0x0.into()), None);
    /// assert_eq!(d.get(0x1000.into()), Some(0x1));
    ///
    /// d.map(0x0.into(), &[0x2; 0x2000]).expect("failed to map");
    ///  assert_eq!(d.get(0x0.into()), Some(0x2));
    ///  assert_eq!(d.get(0x1000.into()), Some(0x2));
    /// ```
    pub fn get(&self, rva: RVA) -> Option<T> {
        if page(rva) > self.pages.len() {
            return None;
        }

        let page = match &self.pages[page(rva)] {
            // page is not mapped
            None => return None,
            // page is mapped
            Some(page) => page,
        };

        Some(page.elements[page_offset(rva)])

    }

    /// handle the simple slice case: when start and end fall within the same page.
    /// for example, reading a dword from address 0x10.
    ///
    /// ```
    /// use lancelot::arch::RVA;
    /// use lancelot::aspace::DenseAddressSpace;
    ///
    /// let mut d: DenseAddressSpace<u32> = DenseAddressSpace::with_capacity(0x2000.into());
    /// d.map_empty(0x0.into(), 0x1000).expect("failed to map");
    /// assert_eq!(d.slice(0x0.into(), 0x2.into()).unwrap(), [0x0, 0x0]);
    /// ```
    fn slice_simple(&self, start: RVA, end: RVA) -> Result<Vec<T>, Error> {
        if page(start) > self.pages.len() {
            return Err(Error::NotMapped);
        }

        let page = match &self.pages[page(start)] {
            // page is not mapped
            None => return Err(Error::NotMapped),
            // page is mapped
            Some(page) => page,
        };

        Ok(page.elements[page_offset(start)..page_offset(end)].to_vec())
    }

    /// handle the complex slice case: when start and end are on different pages.
    /// for example, reading a dword from address 0xFFE.
    ///
    /// ```
    /// use lancelot::arch::RVA;
    /// use lancelot::aspace::DenseAddressSpace;
    ///
    /// let mut d: DenseAddressSpace<u32> = DenseAddressSpace::with_capacity(0x5000.into());
    /// d.map_empty(0x1000.into(), 0x3000).expect("failed to map");
    ///
    /// // 0     unmapped
    /// //       unmapped
    /// // 1000  0 0 0 0
    /// //       0 0 0 0
    /// // 2000  0 0 0 0
    /// //       0 0 0 0
    /// // 3000  0 0 0 0
    /// //       0 0 0 0
    /// // 4000  unmapped
    /// //       unmapped
    /// // 5000  unmapped
    ///
    /// assert_eq!(d.slice(0x1FFC.into(), 0x2000.into()).unwrap(), [0x0, 0x0, 0x0, 0x0], "no overlap");
    /// assert_eq!(d.slice(0x1FFD.into(), 0x2001.into()).unwrap(), [0x0, 0x0, 0x0, 0x0], "overlap 1");
    /// assert_eq!(d.slice(0x1FFE.into(), 0x2002.into()).unwrap(), [0x0, 0x0, 0x0, 0x0], "overlap 2");
    /// assert_eq!(d.slice(0x1FFF.into(), 0x2003.into()).unwrap(), [0x0, 0x0, 0x0, 0x0], "overlap 3");
    /// assert_eq!(d.slice(0x2000.into(), 0x2004.into()).unwrap(), [0x0, 0x0, 0x0, 0x0], "overlap 4");
    ///
    /// assert_eq!(d.slice(0x1FFC.into(), 0x3004.into()).unwrap().len(), 0x1008, "4, page, 4");
    ///
    /// assert_eq!(d.slice(0x1FFC.into(), 0x3000.into()).unwrap().len(), 0x1004, "4, page");
    ///
    /// assert_eq!(d.slice(0x2000.into(), 0x3004.into()).unwrap().len(), 0x1004, "page, 4");
    /// ```
    fn slice_split(&self, start: RVA, end: RVA) -> Result<Vec<T>, Error> {
        // precondition: end > start

        if page(end) > self.pages.len() {
            return Err(Error::NotMapped);
        }

        // ensure each page within the requested region is mapped.
        let start_page = page(start);
        let end_page = if page_offset(end) == 0 { page(end) - 1} else { page(end) };
        for page in start_page..end_page {
            if ! self.probe((page * PAGE_SIZE).into()) {
                return Err(Error::NotMapped);
            }
        }

        let mut ret: Vec<T> = Vec::with_capacity((end - start).into());

        // region one: from `start` to the end of its page
        // region two: any intermediate complete pages
        // region three: from start of final page until `end`

        // one.
        {
            let page = self.pages[page(start)].as_ref().unwrap();
            let buf = &page.elements[page_offset(start)..];
            ret.extend_from_slice(buf);
        }

        // two.
        if page(start) != page(end) - 1 {
            let start_index = page(start) + 1;
            let end_index = page(end);
            for page_index in start_index..end_index {
                let page = self.pages[page_index].as_ref().unwrap();
                let buf = &page.elements[..];
                ret.extend_from_slice(buf);
            }
        }

        // three.
        if page_offset(end) != 0x0 {
            let page = self.pages[page(end)].as_ref().unwrap();
            let buf = &page.elements[..page_offset(end)];
            ret.extend_from_slice(buf);
        }

        Ok(ret)
    }

    /// fetch the items found in the given range.
    ///
    /// errors:
    ///   - Error::NotMapped: if any requested address is not mapped
    ///
    /// panic if:
    ///   - start > end
    pub fn slice(&self, start: RVA, end: RVA) -> Result<Vec<T>, Error> {
        if start > end {
            panic!("start > end");
        }

        if page(start) == page(end) {
            self.slice_simple(start, end)
        } else {
            self.slice_split(start, end)
        }
    }

    // TODO: slice_into
}
