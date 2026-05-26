//! Security-boundary tests for the sandboxed Lua state.
//!
//! Each dangerous global gets its own assertion so a regression names exactly
//! which escape hatch reopened. These run plugin-like chunks in a fresh
//! sandbox and assert the dangerous surface is `nil`/unavailable.

use mote_lua::new_sandbox;

/// Evaluates a boolean Lua expression in a fresh sandbox and returns it.
fn eval_bool(expr: &str) -> bool {
    let lua = new_sandbox().expect("sandbox builds");
    lua.load(expr).eval::<bool>().expect("expr evaluates")
}

#[test]
fn io_library_is_unavailable() {
    assert!(eval_bool("io == nil"), "`io` must be removed");
}

#[test]
fn os_library_is_unavailable() {
    assert!(eval_bool("os == nil"), "`os` must be removed");
}

#[test]
fn debug_library_is_unavailable() {
    assert!(eval_bool("debug == nil"), "`debug` must be removed");
}

#[test]
fn package_and_require_are_unavailable() {
    assert!(eval_bool("package == nil"), "`package` must be removed");
    assert!(eval_bool("require == nil"), "`require` must be removed");
}

#[test]
fn dynamic_code_loading_is_unavailable() {
    // Each its own assertion so a regression names the exact primitive.
    assert!(eval_bool("load == nil"), "`load` must be removed");
    assert!(
        eval_bool("loadstring == nil"),
        "`loadstring` must be removed"
    );
    assert!(eval_bool("loadfile == nil"), "`loadfile` must be removed");
    assert!(eval_bool("dofile == nil"), "`dofile` must be removed");
}

#[test]
fn ffi_library_is_unavailable() {
    // LuaJIT's `ffi` is a native-memory escape hatch; the safe constructor must
    // never load it.
    assert!(eval_bool("ffi == nil"), "`ffi` must be removed");
}

#[test]
fn collectgarbage_is_unavailable() {
    assert!(
        eval_bool("collectgarbage == nil"),
        "`collectgarbage` must be removed"
    );
}

#[test]
fn safe_libraries_are_present() {
    // Sanity check the boundary doesn't over-remove the legitimate surface.
    assert!(eval_bool("string ~= nil"), "`string` must be kept");
    assert!(eval_bool("table ~= nil"), "`table` must be kept");
    assert!(eval_bool("math ~= nil"), "`math` must be kept");
    assert!(eval_bool("coroutine ~= nil"), "`coroutine` must be kept");
    assert!(eval_bool("pcall ~= nil"), "`pcall` must be kept");
    assert!(eval_bool("type ~= nil"), "`type` must be kept");
    assert!(
        eval_bool("setmetatable ~= nil"),
        "`setmetatable` must be kept"
    );
}

#[test]
fn safe_computation_actually_works() {
    let lua = new_sandbox().expect("sandbox builds");
    let n: i64 = lua
        .load("return math.floor(string.len('mote') * 2.5)")
        .eval()
        .expect("safe computation runs");
    assert_eq!(n, 10);
}

#[test]
fn n2_residue_globals_are_unavailable() {
    // Each its own assertion so a regression names the exact primitive.
    assert!(eval_bool("getfenv == nil"), "`getfenv` must be removed");
    assert!(eval_bool("setfenv == nil"), "`setfenv` must be removed");
    assert!(eval_bool("newproxy == nil"), "`newproxy` must be removed");
    assert!(eval_bool("gcinfo == nil"), "`gcinfo` must be removed");
}

#[test]
fn string_dump_is_unavailable() {
    // `string.dump` is a bytecode-leak primitive; it must be nil-ed out of the
    // `string` table even though `string` itself is kept.
    assert!(
        eval_bool("string.dump == nil"),
        "`string.dump` must be removed"
    );
    // The rest of `string` must survive the surgical removal.
    assert!(eval_bool("string.rep ~= nil"), "`string.rep` must be kept");
}

/// **Sandbox surface snapshot (finding N2).** Enumerates every string-keyed
/// global reachable in a fresh sandbox, and the string-keyed fields one level
/// into each top-level table. Pinned so any future mlua/`LuaJIT` widening that
/// reintroduces a global or table field is caught in CI rather than silently
/// granted to plugins.
///
/// If this fails after an intentional change, update `EXPECTED` deliberately —
/// adding surface is a security decision, not a mechanical fix.
#[test]
fn reachable_global_surface_snapshot() {
    // The exact reachable surface of the sandbox. `_G` lists the top-level
    // globals; table entries list their string-keyed fields. Note the denied
    // globals (load, getfenv, …) and `string.dump` are absent.
    const EXPECTED: &str = "\
_G = {_G,_VERSION,assert,bit,coroutine,error,getmetatable,ipairs,jit,math,next,pairs,pcall,print,rawequal,rawget,rawset,select,setmetatable,string,table,tonumber,tostring,type,unpack,xpcall}
_VERSION : string
assert : function
bit = {arshift,band,bnot,bor,bswap,bxor,lshift,rol,ror,rshift,tobit,tohex}
coroutine = {create,isyieldable,resume,running,status,wrap,yield}
error : function
getmetatable : function
ipairs : function
jit = {arch,attach,flush,off,on,opt,os,security,status,version,version_num}
math = {abs,acos,asin,atan,atan2,ceil,cos,cosh,deg,exp,floor,fmod,frexp,huge,ldexp,log,log10,max,min,modf,pi,pow,rad,random,randomseed,sin,sinh,sqrt,tan,tanh}
next : function
pairs : function
pcall : function
print : function
rawequal : function
rawget : function
rawset : function
select : function
setmetatable : function
string = {byte,char,find,format,gmatch,gsub,len,lower,match,rep,reverse,sub,upper}
table = {concat,foreach,foreachi,getn,insert,maxn,move,remove,sort}
tonumber : function
tostring : function
type : function
unpack : function
xpcall : function";

    let lua = new_sandbox().expect("sandbox builds");
    let listing: String = lua
        .load(
            r#"
            local out = {}
            for k, v in pairs(_G) do
              if type(k) == "string" then
                if type(v) == "table" then
                  local fields = {}
                  for fk, _ in pairs(v) do
                    if type(fk) == "string" then fields[#fields + 1] = fk end
                  end
                  table.sort(fields)
                  out[#out + 1] = k .. " = {" .. table.concat(fields, ",") .. "}"
                else
                  out[#out + 1] = k .. " : " .. type(v)
                end
              end
            end
            table.sort(out)
            return table.concat(out, "\n")
        "#,
        )
        .eval()
        .expect("enumeration runs");

    assert_eq!(
        listing, EXPECTED,
        "sandbox reachable surface changed — review whether the new surface is safe before updating EXPECTED"
    );
}
