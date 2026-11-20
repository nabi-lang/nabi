//! The worlds core modules carry in `component-type:*` custom sections
//! (`wit-component`'s binary WIT: a component exporting a component
//! type wrapping the world's type), read into the [`Model`].

use crate::component::bin::{Reader, Res};
use crate::component::types::{Def, Func, Model, ROOT, Tid, Val, World};
use crate::parse::Prim;

/// One entry of a type index space.
#[derive(Clone)]
enum Entry {
    Val(Val),
    Func(Func),
    /// An instance type, read as an interface (index into `Model::ifaces`)
    /// whose key its import or export gives.
    Instance(usize),
}

const HEADER: &[u8] = b"\0asm\x0d\0\x01\0";

/// The worlds of a `component-type:*` section: one per component in it
/// (the linker joins sections of the same name).
pub fn worlds(m: &mut Model, section: &[u8]) -> Res<Vec<World>> {
    let mut r = Reader::new(section);
    let mut out = Vec::new();
    while !r.eof() {
        if r.bytes(HEADER.len())? != HEADER {
            return Err("not a component".to_owned());
        }
        let mut world = None;
        while !r.eof() && !r.b[r.pos..].starts_with(HEADER) {
            let id = r.u8()?;
            let n = r.u32()? as usize;
            let body = r.bytes(n)?;
            if id == 7 && world.is_none() {
                world = Some(wrapper(m, body)?);
            }
        }
        out.push(world.ok_or("no world in a component-type section")?);
    }
    Ok(out)
}

/// The world in a type section: a component type whose one component
/// type declaration is the world's.
fn wrapper(m: &mut Model, body: &[u8]) -> Res<World> {
    let mut s = Reader::new(body);
    for _ in 0..s.u32()? {
        if s.u8()? != 0x41 {
            return Err("unexpected type in a component-type section".to_owned());
        }
        for _ in 0..s.u32()? {
            match s.u8()? {
                0x01 if s.peek()? == 0x41 => {
                    s.u8()?;
                    return world_type(m, &mut s);
                }
                0x04 => {
                    s.u8()?;
                    s.name()?;
                    skip_extern(&mut s)?;
                }
                _ => return Err("unexpected component-type section layout".to_owned()),
            }
        }
    }
    Err("no world in a component-type section".to_owned())
}

fn world_type(m: &mut Model, s: &mut Reader<'_>) -> Res<World> {
    let root = m.iface(ROOT);
    let mut w = World::default();
    let mut types: Vec<Entry> = Vec::new();
    let mut instances: Vec<usize> = Vec::new();
    for _ in 0..s.u32()? {
        match s.u8()? {
            0x00 => skip_core_type(s)?,
            0x01 => {
                let e = deftype(m, s, &types)?;
                types.push(e);
            }
            0x02 => {
                let sort = s.u8()?;
                if sort != 0x03 || s.u8()? != 0x00 {
                    return Err("unsupported alias in a world".to_owned());
                }
                let i = s.u32()? as usize;
                let name = s.name()?;
                let iface = *instances.get(i).ok_or("bad instance index")?;
                let t = m.ifaces[iface]
                    .types
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, t)| *t)
                    .ok_or_else(|| format!("no type `{name}` to alias"))?;
                types.push(Entry::Val(Val::Id(t)));
            }
            k @ (0x03 | 0x04) => {
                let export = k == 0x04;
                s.u8()?;
                let name = s.name()?.to_owned();
                match s.u8()? {
                    0x05 => {
                        let Some(Entry::Instance(i)) = types.get(s.u32()? as usize).cloned() else {
                            return Err("bad instance type index".to_owned());
                        };
                        m.ifaces[i].key = name;
                        instances.push(i);
                        if export {
                            w.exports.push(i);
                        } else {
                            w.imports.push(i);
                        }
                    }
                    0x01 => {
                        let Some(Entry::Func(mut f)) = types.get(s.u32()? as usize).cloned() else {
                            return Err("bad function type index".to_owned());
                        };
                        f.name = name;
                        if export {
                            w.root_exports.push(f);
                        } else {
                            m.ifaces[root].funcs.push(f);
                        }
                    }
                    0x03 => {
                        let t = named_type(m, s, &types, root, &name)?;
                        types.push(Entry::Val(Val::Id(t)));
                    }
                    _ => return Err(format!("unsupported world item `{name}`")),
                }
            }
            _ => return Err("unsupported declaration in a world".to_owned()),
        }
    }
    Ok(w)
}

/// A type bound (`eq` / `sub resource`) named `name` in interface
/// `owner`.
fn named_type(
    m: &mut Model,
    s: &mut Reader<'_>,
    types: &[Entry],
    owner: usize,
    name: &str,
) -> Res<Tid> {
    let t = match s.u8()? {
        0x00 => {
            let v = match types.get(s.u32()? as usize) {
                Some(Entry::Val(v)) => v.clone(),
                _ => return Err(format!("bad type bound of `{name}`")),
            };
            match v {
                Val::Id(t) if m.tys[t].name.is_none() && !matches!(m.tys[t].def, Def::Alias(_)) => {
                    t
                }
                v => m.ty(Def::Alias(v)),
            }
        }
        0x01 => m.ty(Def::Resource),
        _ => return Err(format!("bad type bound of `{name}`")),
    };
    m.tys[t].name = Some((owner, name.to_owned()));
    m.ifaces[owner].types.push((name.to_owned(), t));
    Ok(t)
}

fn instance_type(m: &mut Model, s: &mut Reader<'_>, outer: &[Entry]) -> Res<usize> {
    let id = m.iface("");
    let mut types: Vec<Entry> = Vec::new();
    for _ in 0..s.u32()? {
        match s.u8()? {
            0x00 => skip_core_type(s)?,
            0x01 => {
                let e = deftype(m, s, &types)?;
                types.push(e);
            }
            0x02 => {
                let sort = s.u8()?;
                if sort != 0x03 || s.u8()? != 0x02 || s.u32()? != 1 {
                    return Err("unsupported alias in an interface".to_owned());
                }
                let e = outer
                    .get(s.u32()? as usize)
                    .cloned()
                    .ok_or("bad outer type index")?;
                types.push(e);
            }
            0x04 => {
                s.u8()?;
                let name = s.name()?.to_owned();
                match s.u8()? {
                    0x01 => {
                        let Some(Entry::Func(mut f)) = types.get(s.u32()? as usize).cloned() else {
                            return Err("bad function type index".to_owned());
                        };
                        f.name = name;
                        m.ifaces[id].funcs.push(f);
                    }
                    0x03 => {
                        let t = named_type(m, s, &types, id, &name)?;
                        types.push(Entry::Val(Val::Id(t)));
                    }
                    _ => return Err(format!("unsupported interface item `{name}`")),
                }
            }
            _ => return Err("unsupported declaration in an interface".to_owned()),
        }
    }
    Ok(id)
}

const fn prim(b: u8) -> Option<Val> {
    Some(Val::Prim(match b {
        0x7f => Prim::Bool,
        0x7e => Prim::S8,
        0x7d => Prim::U8,
        0x7c => Prim::S16,
        0x7b => Prim::U16,
        0x7a => Prim::S32,
        0x79 => Prim::U32,
        0x78 => Prim::S64,
        0x77 => Prim::U64,
        0x76 => Prim::F32,
        0x75 => Prim::F64,
        0x74 => Prim::Char,
        0x73 => Prim::String,
        0x64 => return Some(Val::ErrorContext),
        _ => return None,
    }))
}

fn val(s: &mut Reader<'_>, types: &[Entry]) -> Res<Val> {
    if let Some(v) = prim(s.peek()?) {
        s.u8()?;
        return Ok(v);
    }
    let i = s.s33()?;
    match usize::try_from(i).ok().and_then(|i| types.get(i)) {
        Some(Entry::Val(v)) => Ok(v.clone()),
        _ => Err(format!("bad value type index {i}")),
    }
}

fn opt_val(s: &mut Reader<'_>, types: &[Entry]) -> Res<Option<Val>> {
    match s.u8()? {
        0 => Ok(None),
        1 => val(s, types).map(Some),
        _ => Err("bad optional type".to_owned()),
    }
}

fn labels(s: &mut Reader<'_>) -> Res<Vec<String>> {
    (0..s.u32()?).map(|_| Ok(s.name()?.to_owned())).collect()
}

fn resource(s: &mut Reader<'_>, types: &[Entry]) -> Res<Tid> {
    match types.get(s.u32()? as usize) {
        Some(Entry::Val(Val::Id(t))) => Ok(*t),
        _ => Err("bad handle type".to_owned()),
    }
}

fn deftype(m: &mut Model, s: &mut Reader<'_>, types: &[Entry]) -> Res<Entry> {
    let b = s.u8()?;
    if let Some(v) = prim(b) {
        return Ok(Entry::Val(v));
    }
    let def = match b {
        0x72 => Def::Record(
            (0..s.u32()?)
                .map(|_| Ok((s.name()?.to_owned(), val(s, types)?)))
                .collect::<Res<_>>()?,
        ),
        0x71 => Def::Variant(
            (0..s.u32()?)
                .map(|_| {
                    let n = s.name()?.to_owned();
                    let t = opt_val(s, types)?;
                    if s.u8()? != 0 {
                        return Err("variant refinements are not supported".to_owned());
                    }
                    Ok((n, t))
                })
                .collect::<Res<_>>()?,
        ),
        0x70 => Def::List(val(s, types)?),
        0x67 => {
            let e = val(s, types)?;
            Def::FixedList(e, s.u32()?)
        }
        0x6f => Def::Tuple((0..s.u32()?).map(|_| val(s, types)).collect::<Res<_>>()?),
        0x6e => Def::Flags(labels(s)?),
        0x6d => Def::Enum(labels(s)?),
        0x6b => Def::Option(val(s, types)?),
        0x6a => {
            let a = opt_val(s, types)?;
            Def::Result(a, opt_val(s, types)?)
        }
        0x69 => Def::Own(resource(s, types)?),
        0x68 => Def::Borrow(resource(s, types)?),
        0x66 => Def::Stream(opt_val(s, types)?),
        0x65 => Def::Future(opt_val(s, types)?),
        0x40 | 0x43 => {
            let params = (0..s.u32()?)
                .map(|_| Ok((s.name()?.to_owned(), val(s, types)?)))
                .collect::<Res<_>>()?;
            let result = match s.u8()? {
                0x00 => Some(val(s, types)?),
                0x01 if s.u8()? == 0 => None,
                _ => return Err("bad function result".to_owned()),
            };
            return Ok(Entry::Func(Func {
                name: String::new(),
                is_async: b == 0x43,
                params,
                result,
            }));
        }
        0x42 => return instance_type(m, s, types).map(Entry::Instance),
        _ => {
            return Err(format!(
                "unsupported type {b:#04x} in a component-type section"
            ));
        }
    };
    Ok(Entry::Val(Val::Id(m.ty(def))))
}

fn skip_extern(s: &mut Reader<'_>) -> Res<()> {
    match s.u8()? {
        0x00 => {
            s.u8()?;
            s.u32()?;
        }
        0x01 | 0x04 | 0x05 => {
            s.u32()?;
        }
        0x03 => {
            if s.u8()? == 0 {
                s.u32()?;
            }
        }
        _ => return Err("unsupported extern type".to_owned()),
    }
    Ok(())
}

fn skip_core_type(s: &mut Reader<'_>) -> Res<()> {
    match s.u8()? {
        0x60 => {
            for _ in 0..2 {
                let n = s.u32()? as usize;
                s.bytes(n)?;
            }
            Ok(())
        }
        _ => Err("unsupported core type in a component-type section".to_owned()),
    }
}
