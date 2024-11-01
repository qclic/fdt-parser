use core::iter;

use crate::{
    clocks::{ClockRef, ClocksIter},
    error::{FdtError, FdtResult},
    interrupt::{InterruptController, InterruptInfo},
    meta::MetaData,
    property::Property,
    read::FdtReader,
    Fdt, FdtRange, FdtRangeSilce, FdtReg, Phandle, Token,
};

#[derive(Clone)]
pub struct Node<'a> {
    pub level: usize,
    pub name: &'a str,
    pub(crate) fdt: &'a Fdt<'a>,
    /// 父节点的元数据
    pub(crate) meta_parents: MetaData<'a>,
    /// 当前节点的元数据
    pub(crate) meta: MetaData<'a>,
    body: FdtReader<'a>,
}

impl<'a> Node<'a> {
    pub(crate) fn new(
        fdt: &'a Fdt<'a>,
        level: usize,
        name: &'a str,
        reader: FdtReader<'a>,
        meta_parents: MetaData<'a>,
        meta: MetaData<'a>,
    ) -> Self {
        Self {
            fdt,
            level,
            body: reader,
            name,
            meta,
            meta_parents,
        }
    }

    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn propertys(&self) -> impl Iterator<Item = Property<'a>> + '_ {
        let reader = self.body.clone();
        PropIter {
            reader,
            fdt: self.fdt,
        }
    }

    pub fn find_property(&self, name: &str) -> Option<Property<'a>> {
        self.propertys().find(|x| x.name.eq(name))
    }

    pub fn reg(&self) -> Option<impl Iterator<Item = FdtReg> + 'a> {
        let mut iter = self.propertys();
        let reg = iter.find(|x| x.name.eq("reg"))?;

        Some(RegIter {
            size_cell: self.size_cells().unwrap(),
            prop: reg,
            node: self.clone(),
        })
    }

    fn address_cells(&self) -> Option<u8> {
        if let Some(a) = self.meta.address_cells {
            return Some(a);
        }
        self.meta_parents.address_cells
    }

    fn size_cells(&self) -> Option<u8> {
        if let Some(a) = self.meta.size_cells {
            return Some(a);
        }
        self.meta_parents.size_cells
    }

    pub fn ranges(&self) -> impl Iterator<Item = FdtRange> + 'a {
        let mut iter = self.meta.range.clone().map(|m| m.iter());
        if iter.is_none() {
            iter = self.meta_parents.range.clone().map(|m| m.iter());
        }

        iter::from_fn(move || match &mut iter {
            Some(i) => i.next(),
            None => None,
        })
    }

    pub(crate) fn node_ranges(&self) -> Option<FdtRangeSilce<'a>> {
        let prop = self.find_property("ranges")?;

        Some(FdtRangeSilce::new(
            self.meta.address_cells.unwrap(),
            self.meta_parents.address_cells.unwrap(),
            self.meta.size_cells.unwrap(),
            prop.data.clone(),
        ))
    }

    pub(crate) fn node_interrupt_parent(&self) -> Option<Phandle> {
        let prop = self.find_property("interrupt-parent")?;
        Some(prop.u32().into())
    }

    /// Find [InterruptController] from current node or its parent
    pub fn interrupt_parent(&self) -> Option<InterruptController<'a>> {
        let phandle = if let Some(p) = self.meta.interrupt_parent {
            Some(p)
        } else {
            self.meta_parents.interrupt_parent
        }?;

        self.fdt
            .get_node_by_phandle(phandle)
            .map(|node| InterruptController { node })
    }

    pub fn compatible(&self) -> Option<impl Iterator<Item = FdtResult<'a, &'a str>> + 'a> {
        let prop = self.find_property("compatible")?;
        let mut value = prop.data.clone();

        Some(iter::from_fn(move || {
            let s = value.take_str();
            match s {
                Ok(s) => {
                    if s.is_empty() {
                        None
                    } else {
                        Some(Ok(s))
                    }
                }
                Err(e) => match e {
                    FdtError::Eof => None,
                    _ => Some(Err(e)),
                },
            }
        }))
    }

    /// Get all compatible ignoring errors
    pub fn compatibles(&self) -> impl Iterator<Item = &'a str> + 'a {
        let mut cap_raw = self.compatible();

        iter::from_fn(move || {
            if let Some(caps) = &mut cap_raw {
                let cap = caps.next()?.ok()?;
                Some(cap)
            } else {
                None
            }
        })
    }

    pub fn phandle(&self) -> Option<Phandle> {
        let prop = self.find_property("phandle")?;
        Some(prop.u32().into())
    }

    pub fn interrupts(&self) -> Option<InterruptInfo> {
        let prop = self.find_property("interrupts")?;
        let cell_size = self.interrupt_parent()?.interrupt_cells();

        Some(InterruptInfo { cell_size, prop })
    }

    pub fn clocks(&'a self) -> impl Iterator<Item = ClockRef<'a>> + 'a {
        ClocksIter::new(self)
    }

    pub fn clock_frequency(&self) -> Option<u32> {
        let prop = self.find_property("clock-frequency")?;
        Some(prop.u32())
    }
}

struct RegIter<'a> {
    size_cell: u8,
    prop: Property<'a>,
    node: Node<'a>,
}
impl<'a> Iterator for RegIter<'a> {
    type Item = FdtReg;

    fn next(&mut self) -> Option<Self::Item> {
        let child_address_cell = self.node.address_cells().unwrap();
        let child_bus_address = self.prop.data.take_by_cell_size(child_address_cell)?;

        let mut address = child_bus_address;
        for one in self.node.ranges() {
            if child_bus_address >= one.child_bus_address
                && child_bus_address < one.child_bus_address + one.size as u128
            {
                address = child_bus_address - one.child_bus_address + one.parent_bus_address;
            }
        }

        let size = if self.size_cell > 0 {
            Some(self.prop.data.take_by_cell_size(self.size_cell)? as usize)
        } else {
            None
        };
        Some(FdtReg {
            address,
            child_bus_address,
            size,
        })
    }
}

struct PropIter<'a> {
    fdt: &'a Fdt<'a>,
    reader: FdtReader<'a>,
}

impl<'a> Iterator for PropIter<'a> {
    type Item = Property<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.reader.take_token() {
                Some(token) => match token {
                    Token::Prop => break,
                    Token::Nop => {}
                    _ => return None,
                },
                None => return None,
            }
        }
        self.reader.take_prop(self.fdt)
    }
}

// #[derive(Clone)]
// pub struct MemoryRegionSilce<'a> {
//     address_cell: u8,
//     size_cell: u8,
//     reader: FdtReader<'a>,
// }

// impl<'a> MemoryRegionSilce<'a> {
//     pub fn iter(&self) -> impl Iterator<Item = FdtRange> + 'a {
//         MemoryRegionIter {
//             address_cell: self.address_cell,
//             size_cell: self.size_cell,
//             reader: self.reader.clone(),
//         }
//     }
// }

// struct MemoryRegionIter<'a> {
//     address_cell: u8,
//     size_cell: u8,
//     reader: FdtReader<'a>,
// }

// impl<'a> Iterator for MemoryRegionIter<'a> {
//     type Item = FdtRange;

//     fn next(&mut self) -> Option<Self::Item> {
//         todo!()
//     }
// }
