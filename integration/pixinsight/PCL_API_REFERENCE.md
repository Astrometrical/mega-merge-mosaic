# PCL C++ API Reference — for mmm-pxm (PixInsight native module)

Researched against the local PixInsight install: headers at `/opt/PixInsight/include/pcl/`
(PCL 2.10.4, released 2026-06-21), sources at `/opt/PixInsight/src/pcl/`. All signatures below
are copied or closely paraphrased from those files with file+line citations. **No complete
example module (`.cpp`) exists anywhere under `/opt/PixInsight`** — only the PCL library itself.
Every code skeleton below is therefore *synthesized* from the header doc comments and confirmed
constructor/implementation bodies, not copied from a working module; the plan author should
sanity-check skeletons against the open-source PCL repo (gitlab.com/pixinsight/PCL) or any
third-party module source if available, before relying on them verbatim.

> **Build-pin caveat:** CI builds the module against PCL **2.8.3** (PixInsight
> 1.9.0), pinned in `ci/pcl-pin.env`, so the module's declared
> `PCL_API_Version` (0x0182) admits every 1.9.x core — see the policy comment
> in the pin file. (Exception: the macos-arm64 leg builds against 2.10.4,
> the first arm64-capable PCL; every arm64 core is ≥ 1.9.4.) This document's
> file+line citations refer to PCL 2.10.4 headers; the entry-point/ABI facts
> below are stable across that span, but any *new* API must be verified
> against the pinned 2.8.3 headers before use in the module — 2.8.3 is what
> most targets compile against.

Target: Linux, one `mmm-pxm.so`, one global-context `MetaProcess`/`ProcessImplementation`, one
`ProcessInterface`, no PJSR/JavaScript.

---

## 1. Module registration / entry point

Files read in full: `pcl/MetaModule.h` (1036 lines), `pcl/MetaObject.h` (162 lines),
`src/pcl/MetaModule.cpp` (444 lines).

### `PCL_MODULE_EXPORT` macro

`pcl/Defs.h:380,391,393`:
```cpp
// MSVC
#  define PCL_MODULE_EXPORT   extern "C" PCL_EXPORT
// clang, Linux/macOS
#    define PCL_MODULE_EXPORT   extern "C" __attribute__((visibility ("default")))
// gcc, Linux/macOS
#    define PCL_MODULE_EXPORT   extern "C" __attribute__((visibility ("default"), externally_visible))
```
On Linux/gcc this forces `extern "C"` + default visibility, so the symbol is exported from the
`.so` even though PCL modules are normally built with `-fvisibility=hidden`.

### Three entry-point functions (`pcl/MetaModule.h:895-1027`, doxygen group `module_entry_points`)

These are free functions with C linkage — **not** members of `MetaModule`. PMIDN and PMINI are
implemented internally by PCL's own generated glue (you never write them); a hand-written module
normally supplies only PMINS:

```cpp
// PMIDN — module identification (PCL supplies this)
PCL_MODULE_EXPORT uint32 IdentifyPixInsightModule( api_module_description** description, int32 phase );

// PMINI — module initialization / API handshake (PCL supplies this)
PCL_MODULE_EXPORT uint32 InitializePixInsightModule( api_handle hModule, function_resolver R, uint32 apiVersion, void* reserved );

// PMINS — module installation (YOU write this)
PCL_MODULE_EXPORT int32 InstallPixInsightModule( int32 mode );
```
`mode` is one of `pcl::InstallMode::{FullInstall, QueryModuleInfo, VerifyModule}`
(`MetaModule.h:720-728`). Doc for PMINS (`MetaModule.h:1017-1021`): "normally has to be defined
by non-trivial modules in order to create and initialize the different objects and meta-objects
required to implement their functionality, since most of these objects are dynamic in PCL."

### `MetaModule` — required/optional overrides

Pure virtual (must implement), `MetaModule.h:163,224`:
```cpp
virtual const char* Version() const = 0;   // "PIXINSIGHT_MODULE_VERSION_<version-string>"
virtual IsoString    Name() const = 0;     // unique, valid-C-identifier module id
```
Build `Version()` with the provided macro (`MetaModule.h:807-813`):
```cpp
#define PCL_MODULE_VERSION( MM, mm, rr, bbbb, lan ) \
   ("PIXINSIGHT_MODULE_VERSION_" PCL_STRINGIFY(MM) "." PCL_STRINGIFY(mm) "." \
    PCL_STRINGIFY(rr) "." PCL_STRINGIFY(bbbb) "." PCL_STRINGIFY(lan))
```
(there's also `PCL_MODULE_VERSION_S(..., status)` adding a status word, lines 880-887).

Optional overrides with empty/no-op default bodies (all `virtual`):
`Description()` (244), `Company()` (259), `Author()` (271), `Copyright()` (284),
`TradeMarks()` (296), `OriginalFileName()` (306), `GetReleaseDate(int&,int&,int&)` (329),
`Allocate(size_type)` (433), `Deallocate(void*)` (454), `OnLoad()` (468), `OnUnload()` (482).
There is **no** TIFF-specific hook on `MetaModule` — format-specific behavior belongs to
`MetaFileFormat`, unrelated to this task.

Deprecated, do not use: `virtual const char* UniqueId() const;` (102).

Private/framework-only: `void PerformAPIDefinitions() const override;` (683, friended to
`APIInitializer` — where actual API registration happens; not user-overridden).

### Self-registration idiom (confirmed from `.cpp` source, not just docs)

`MetaObject`'s constructor takes a parent and links itself into the parent's children list
(`MetaObject.h:66-71`):
```cpp
MetaObject( MetaObject* parent ) : m_parent( parent )
{
   if ( m_parent != nullptr )
      m_parent->m_children.Add( this );
}
```
`MetaModule` is the tree root and sets the extern global on construction
(`src/pcl/MetaModule.cpp:58-68`):
```cpp
MetaModule* Module = nullptr;
MetaModule::MetaModule() : MetaObject( nullptr )
{
   if ( Module != nullptr )
      throw Error( "MetaModule: Module redefinition not allowed" );
   Module = this;
}
```
`extern MetaModule* Module;` is declared at `MetaModule.h:690` — this global is reachable from
anywhere in your module's code.

`MetaProcess` and `ProcessInterface` both construct with `Module` as their `MetaObject` parent,
so **constructing them is what registers them** — confirmed in real `.cpp` sources:
```cpp
// src/pcl/MetaProcess.cpp:47-48
MetaProcess::MetaProcess() : MetaObject( Module ) { ... }

// src/pcl/ProcessInterface.cpp:39-44
ProcessInterface::ProcessInterface() : Control( nullptr ), MetaObject( Module )
{
   if ( Module == nullptr )
      throw Error( "ProcessInterface: Module not initialized - illegal ProcessInterface instantiation" );
}
```
This is why `InstallPixInsightModule()` is where you `new` the process/interface/parameter
singletons: `Module` (your `MetaModule` instance) must exist first, and `new MyProcess` /
`new MyInterface` self-register into it automatically. There is no manual "add to module" call.
Likewise, each `MetaParameter` self-registers under its owning `MetaProcess*` (or `MetaTable*`)
via the same mechanism (see §3).

### Skeletal module `.cpp` (synthesized)

```cpp
#include <pcl/MetaModule.h>

#define MODULE_VERSION_MAJOR     01
#define MODULE_VERSION_MINOR     00
#define MODULE_VERSION_REVISION  00
#define MODULE_VERSION_BUILD     0001
#define MODULE_VERSION_LANGUAGE  eng

class MmmModule : public pcl::MetaModule
{
public:
   MmmModule() = default;

   const char* Version() const override
   {
      return PCL_MODULE_VERSION( MODULE_VERSION_MAJOR, MODULE_VERSION_MINOR,
                                  MODULE_VERSION_REVISION, MODULE_VERSION_BUILD,
                                  MODULE_VERSION_LANGUAGE );
   }
   pcl::IsoString Name() const override { return "MergeMosaic"; }
   pcl::String Description() const override { return "MergeMosaic Blend Process Module"; }
};

static MmmModule* TheMmmModule = nullptr;

// Declared in MmmProcess.h / MmmInterface.h — see §2-4
extern pcl::MetaProcess*      TheMmmBlendProcess;
extern pcl::ProcessInterface* TheMmmBlendInterface;

PCL_MODULE_EXPORT int32 InstallPixInsightModule( int32 mode )
{
   new MmmModule;   // sets pcl::Module

   if ( mode == pcl::InstallMode::FullInstall )
   {
      new MmmBlendProcess;     // self-registers under Module
      new MmmBlendInterface;   // self-registers under Module
      // MetaParameter subclass instances (TheXxxParameter globals) self-register
      // under their owning MetaProcess*/MetaTable* the same way — see §3.
   }
   return 0;
}
```

**Gotcha**: `ProcessImplementation` (and `MetaProcess`) have *no pure-virtual methods in the C++
sense* for most of the "must override" list below — several compile fine with default bodies
that deliberately `throw` at runtime if you forget to override them. Getting these wrong is a
runtime failure, not a compile error (see §2).

---

## 2. MetaProcess + ProcessImplementation (global-context process)

Files read in full: `pcl/MetaProcess.h` (1034), `pcl/ProcessImplementation.h` (879),
`pcl/ProcessBase.h` (267). `pcl/Process.h` is the client-side handle other modules use to call
an *already-installed* process — irrelevant to implementing your own.

`class MetaProcess : public MetaObject, public ProcessBase` (`MetaProcess.h:75`).

### Identity / instance-creation — must implement

```cpp
virtual IsoString Id() const override = 0;                                    // MetaProcess.h:116
virtual ProcessImplementation* Create() const = 0;                            // MetaProcess.h:670
virtual ProcessImplementation* Clone( const ProcessImplementation& ) const = 0;// MetaProcess.h:683
```

### Optional, with defaults

```cpp
virtual IsoString Aliases() const { return IsoString(); }                 // 142
virtual IsoString Categories() const { return IsoString(); }              // 172 (comma-separated; empty = <Etc>)
virtual IsoString Category() const { return Categories(); }               // 184, deprecated synonym
uint32 Version() const override { return 0x100; }                         // 203, hex M.m.r e.g. 0x105=1.0.5
String Description() const override { return String(); }                  // 227
String ScriptComment() const override { return String(); }                // 243
virtual IsoString IconImageSVG() const { return IsoString(); }             // 273, preferred icon format
virtual String    IconImageSVGFile() const { return String(); }           // 340, supports "@module_icons_dir/"
virtual const char** IconImageXPM() const { return nullptr; }             // 371, deprecated since 1.8.8-6
virtual void InitializeClass() {}                                          // 484, called once module fully installed
virtual ProcessInterface* DefaultInterface() const { return nullptr; }    // 513, interface launched on instance-less activation
virtual ProcessImplementation* TestClone( const ProcessImplementation& p ) const { return Clone( p ); } // 710
```

### Global vs per-view — the exact mechanism

`MetaProcess` capability flags (class-level; both default `true` "for compatibility with
previous PCL versions", explicitly called "clearly suboptimal" by the header — `MetaProcess.h:551-557`):
```cpp
bool CanProcessViews() const override { return true; }    // 534
bool CanProcessGlobal() const override { return true; }   // 559
bool PrefersGlobalExecution() const override { return false; } // 801
```
For **mmm-pxm (global-only, no target view)**: override `CanProcessViews()` to `false` and
`PrefersGlobalExecution()` to `true`; leave/override `CanProcessGlobal()` `true`.

The actual per-instance gating the core calls at runtime lives on `ProcessImplementation`:
```cpp
virtual bool CanExecuteGlobal( String& whyNot ) const;                     // ProcessImplementation.h:449
virtual bool ExecuteGlobal();                                              // ProcessImplementation.h:487
virtual bool CanExecuteOn( const View& view, String& whyNot ) const;       // ProcessImplementation.h:212, default true
virtual bool ExecuteOn( View& view );                                      // ProcessImplementation.h:327, default throws
```
Doc for `CanExecuteGlobal` (419-448): "Execution in the global context occurs when a process
instance is executed without any specific target view... A good example of a *pure global*
process is the GlobalPreferences process... The default implementation of this function returns
false... So by default a process instance **cannot** be executed in the global context." →
**must override to return `true`** for mmm-pxm.

Doc for `ExecuteGlobal` (483-485, confirmed as deliberate): "the default implementation...
throws a runtime exception. This has been done on purpose to recall you that this function must
be reimplemented for processes with global execution capabilities."

For a global-only process, also override `CanExecuteOn(const View&, String&)` to return `false`
(its default is `true`) so the core never offers per-view execution your module doesn't
implement — otherwise `ExecuteOn(View&)`'s "throws on purpose" default fires at runtime instead
of being cleanly refused.

`IsAssignable()` (`MetaProcess.h:716-730`, `ProcessBase.h:184-190`, default `true`) — override to
`false` only if the process has no parameters at all (not our case).

### `ExecuteGlobal` signature (the key method to implement)

```cpp
virtual bool ExecuteGlobal();   // ProcessImplementation.h:487
```
Returns `true` on success. Called with no target view — this is where mmm-pxm enumerates
selected views, spawns the helper binary, streams shared memory, and builds the output window.

### ProcessImplementation base class

```cpp
ProcessImplementation( const MetaProcess* m ) : meta( m ) {}       // ProcessImplementation.h:74-77
ProcessImplementation( const ProcessImplementation& ) = default;   // 82
virtual ~ProcessImplementation() {}                                // 87
const MetaProcess* Meta() const { return meta; }                   // 96-99
protected:
   const MetaProcess* meta = nullptr;                              // 856
```

Other relevant virtuals, all with default bodies (none pure-virtual in the C++ sense; several
`throw` on purpose if unreimplemented — see comment above):
```cpp
virtual void Initialize();                               // 134, only if MetaProcess::NeedsInitialization()==true
virtual bool Validate( String& info );                    // 154, only if NeedsValidation()==true
virtual void Assign( const ProcessImplementation& );      // 177, mandatory for assignable processes
virtual bool BeforeGlobalExecution();                     // 463, default true; last chance to cancel
virtual void AfterGlobalExecution() {}                     // 496, optional cleanup
virtual ProcessInterface* SelectInterface() const { return meta->DefaultInterface(); }         // 519-523
virtual bool IsValidInterface( const ProcessInterface* i ) const { return i == SelectInterface(); } // 537-540
```
Per-image methods (irrelevant unless `MetaProcess::CanProcessImages()` also overridden true,
default false): `CanExecuteOn(const ImageVariant&, String&)` (373), `ExecuteOn(ImageVariant&, const IsoString&)` (417).

### Parameter storage plumbing (mandatory for any process with parameters — see §3)

```cpp
virtual void*     LockParameter( const MetaParameter* p, size_type tableRow );        // 638
virtual void      UnlockParameter( const MetaParameter* p, size_type tableRow );       // 660
virtual bool      ValidateParameter( void* value, const MetaParameter* p, size_type tableRow ) const; // 681
virtual bool      AllocateParameter( size_type sizeOrLength, const MetaParameter* p, size_type tableRow ); // 705
virtual size_type ParameterLength( const MetaParameter* p, size_type tableRow ) const; // 725
```
Base-class bodies (`src/pcl/ProcessImplementation.cpp:34-41,119-154`) all call a `MANDATORY()`
macro that throws `"...must be reimplemented in descendant class."` — i.e. skipping these is a
runtime crash the first time the core touches a parameter, not a compile error.

### Parameter enumeration/lookup

There is **no integer-indexed value accessor**. Two separate mechanisms:
1. `MetaProcess::operator[]( size_type i ) const` (`MetaProcess.h:867`) walks the *meta*-level
   child `MetaParameter`s — "intended for internal use of API module initialization routines...
   you should not need to call this function directly."
2. At the *instance* level, the core looks up storage by **pointer identity of the
   `MetaParameter*`**, via `LockParameter(p, tableRow)`. The idiomatic PCL pattern (implied by
   the pointer-identity dispatch design, PCL's standard "Parameters.h/.cpp" convention): each
   `MetaParameter` subclass is instantiated once as a global singleton (e.g.
   `TheMmmThresholdParameter`), and `LockParameter` does `if ( p == TheMmmThresholdParameter )
   return &instance->threshold; ...` chains.

---

## 3. Process parameters (MetaFloat/MetaInt32/MetaBoolean/MetaString/MetaEnumeration/MetaTable)

File read in full: `pcl/MetaParameter.h` (1817 lines) + `src/pcl/MetaParameter.cpp` (410 lines).
**No example module uses `MetaTable`/`MetaEnumeration` anywhere in the shipped tree** — only the
abstract declarations. Class hierarchy:

```
MetaParameter
├── MetaNumeric
│   ├── MetaInteger
│   │   ├── MetaUnsignedInteger → MetaUInt8/16/32/64
│   │   └── MetaSignedInteger   → MetaInt8/16/32/64
│   └── MetaReal → MetaFloat, MetaDouble
├── MetaBoolean
├── MetaEnumeration
└── MetaVariableLengthParameter
    ├── MetaString
    ├── MetaTable
    └── MetaBlock
```

### Base constructors — the process-vs-table-column duality

`MetaParameter.h:75,82`:
```cpp
MetaParameter( MetaProcess* P );   // parameter of a process
MetaParameter( MetaTable* T );     // column of a table — appended to T's column list
```
**Every intermediate/leaf class repeats both overloads** (e.g. `MetaString.h:1580,1591`). This is
exactly the mechanism for a table column: declare an ordinary leaf type whose constructor is
invoked with a `MetaTable*` instead of a `MetaProcess*`.

### MetaFloat / MetaDouble (`MetaParameter.h:374,984,1089,1133`)

`MetaNumeric` overrides (all `virtual double`, `MetaParameter.h:435-462`):
```cpp
virtual double DefaultValue() const { return 0; }
virtual double MinimumValue() const { return -DBL_MAX; }
virtual double MaximumValue() const { return +DBL_MAX; }
```
`MetaReal` adds (1045-1066):
```cpp
virtual int  Precision() const { return -1; }           // -1 = printf %g; else # decimal digits (max 7 for float, 16 for double)
virtual bool ScientificNotation() const { return false; }
```
`MetaFloat`/`MetaDouble` themselves add nothing but `Id()` and a fixed private `APIParType()`.
Concrete subclass must override: `Id()` (mandatory), `DefaultValue()`, `MinimumValue()`,
`MaximumValue()`, optionally `Precision()`/`ScientificNotation()`.

```cpp
class MmmThresholdParameter : public pcl::MetaFloat
{
public:
   MmmThresholdParameter( pcl::MetaProcess* P ) : MetaFloat( P ) {}
   IsoString Id() const override { return "threshold"; }
   double DefaultValue() const override { return 0.5; }
   double MinimumValue() const override { return 0.0; }
   double MaximumValue() const override { return 1.0; }
};
```

### MetaInt32 / MetaUInt32 (`MetaParameter.h:486,592/542,898/728`)

Add nothing beyond `Id()` and fixed `APIParType()`. Same overrides as float
(`DefaultValue()`/`MinimumValue()`/`MaximumValue()`, still declared `double` even for integers —
the framework rounds/truncates per `MetaNumeric`).

### MetaBoolean (`MetaParameter.h:1184-1234`)

```cpp
class MetaBoolean : public MetaParameter
{
public:
   MetaBoolean( MetaProcess* P ) : MetaParameter( P ) {}
   MetaBoolean( MetaTable* T )   : MetaParameter( T ) {}
   bool IsBoolean() const override { return true; }
   virtual bool DefaultValue() const { return false; }
   virtual IsoString Id() const override = 0;
};
```
**Important** (1175-1180): "Boolean process parameters must be implemented as 32-bit signed
integers (int32)... The best way to implement Boolean process parameters is by using the
`pcl_bool` class." `pcl_bool` (1254-1326) wraps an `int32 m_value` with implicit `bool`/`int`
conversions — use it for the `ProcessImplementation` storage member (`pcl_bool p_someFlag =
false;`), not plain `bool`.

### MetaEnumeration (`MetaParameter.h:1347-1469`)

```cpp
class MetaEnumeration : public MetaParameter
{
public:
   MetaEnumeration( MetaProcess* P ) : MetaParameter( P ) {}
   MetaEnumeration( MetaTable* T )   : MetaParameter( T ) {}
   bool IsEnumeration() const override { return true; }
   virtual IsoString Id() const override = 0;

   virtual size_type NumberOfElements() const = 0;            // must be > 0
   virtual IsoString ElementId( size_type idx ) const = 0;     // unique C-identifier per element
   virtual int       ElementValue( size_type idx ) const = 0;  // the STORED numeric value (not the index)
   virtual size_type DefaultValueIndex() const { return 0; }   // index 0..n-1, NOT a value
   virtual IsoString ElementAliases() const { return IsoString(); } // "alias_id=element_id,..."
};
using pcl_enum = int32;   // MetaParameter.h:1484 — the recommended storage type
```
Key distinction (1400-1414): index (0..n-1, position) vs. value (the stored `int`, can be
non-contiguous) — `ElementValue(idx)` returns the value, and this **value** (not the index)
round-trips through the core (`MetaParameter.cpp:284-290`: `DefineEnumerationElement(id.c_str(),
ElementValue(i))`). Same 32-bit-signed-int storage warning as booleans (1338-1343) — use
`pcl_enum` for the backing member.

```cpp
class MmmModeParameter : public pcl::MetaEnumeration
{
public:
   enum { Average, Weighted, NumberOfModes, Default = Average };
   MmmModeParameter( pcl::MetaProcess* P ) : MetaEnumeration( P ) {}
   IsoString Id() const override { return "blendMode"; }
   size_type NumberOfElements() const override { return NumberOfModes; }
   IsoString ElementId( size_type i ) const override
   { return i == Average ? "Average" : "Weighted"; }
   int ElementValue( size_type i ) const override { return int( i ); }
   size_type DefaultValueIndex() const override { return Default; }
};
```

### MetaString (`MetaParameter.h:1572-1641`)

Derives from `MetaVariableLengthParameter`, not `MetaParameter` directly:
```cpp
class MetaString : public MetaVariableLengthParameter
{
public:
   MetaString( MetaProcess* P ) : MetaVariableLengthParameter( P ) {}
   MetaString( MetaTable* T )   : MetaVariableLengthParameter( T ) {}
   bool IsString() const override { return true; }
   virtual IsoString Id() const override = 0;
   virtual String DefaultValue() const { return String(); }
   virtual String AllowedCharacters() const { return String(); }  // empty = any char valid
};
```
Inherits from `MetaVariableLengthParameter` (1495-1561, shared with `MetaTable`/`MetaBlock`):
```cpp
virtual size_type MinLength() const { return 0; }  // can be empty
virtual size_type MaxLength() const { return 0; }  // 0 = unlimited length
```
Strings are UTF-16 (doc comment 1569-1570: "zero-terminated 16-bit Unicode characters").
`MetaParameter.cpp:334-337` only calls `SetStringLengthLimits` if either bound is nonzero.

### MetaTable — the view-id LIST mechanism (`MetaParameter.h:1657-1697`)

```cpp
class MetaTable : public MetaVariableLengthParameter
{
public:
   MetaTable( MetaProcess* P ) : MetaVariableLengthParameter( P ) {}
   bool IsTable() const override { return true; }
   virtual IsoString Id() const override = 0;
   const MetaParameter* operator[]( size_type i ) const;   // column accessor by index
private:
   MetaTable( MetaTable* );   // nested tables forbidden — see below
};
```
Doc (1653-1655): "Table process parameters cannot be nested, that is, a table process parameter
cannot be specified as a column of an existing table process parameter." The private
`MetaTable(MetaTable*)` constructor exists **only to throw** at runtime
(`src/pcl/MetaParameter.cpp:60-64`):
```cpp
MetaTable::MetaTable( MetaTable* t ) : MetaVariableLengthParameter( t )
{
   throw Error( "MetaTable: Nested tables not allowed" );
}
```
`MinLength()`/`MaxLength()` (inherited) mean **row-count** bounds for a table, confirmed at
`MetaParameter.cpp:358-361` (`SetTableRowLimits(minl, maxl)`).

**Gotcha #1**: `MetaTable::Length()`/`operator[]` (inherited from `MetaObject`/defined for
tables) walk the table's **column** metaparameters (registered once at module-load time), **not**
row count — confirmed at `MetaParameter.cpp:349-356`, which iterates `Length()` columns to call
`PerformAPIDefinitions()` on each. Row count is a purely runtime, per-instance quantity (see
below).

**The exact pattern for our view-id list**: one `MetaTable` singleton (e.g.
`TheInputImagesParameter`) owning one `MetaString` column (e.g. `TheImageViewIdParameter`)
constructed with the table as parent:

```cpp
class MmmInputImagesParameter : public pcl::MetaTable
{
public:
   MmmInputImagesParameter( pcl::MetaProcess* P ) : MetaTable( P ) {}
   IsoString Id() const override { return "inputImages"; }
   size_type MinLength() const override { return 2; }   // require >= 2 views to blend
};

class MmmImageViewIdParameter : public pcl::MetaString
{
public:
   MmmImageViewIdParameter( pcl::MetaTable* T ) : MetaString( T ) {}  // parent is the TABLE
   IsoString Id() const override { return "viewId"; }
};

// singletons, constructed in dependency order in InstallPixInsightModule():
static MmmInputImagesParameter*  TheMmmInputImagesParameter  = nullptr; // new'd first
static MmmImageViewIdParameter*  TheMmmImageViewIdParameter  = nullptr; // new'd with TheMmmInputImagesParameter
```

### How ProcessImplementation actually stores/exposes the list — the crux

`MetaParameter` subclasses are pure metadata; **actual per-instance values live in your
`ProcessImplementation` subclass as ordinary C++ members** (e.g. `Array<String> p_viewIds;`),
exposed to the core exclusively through the five virtual hooks from §2, keyed by
`(const MetaParameter* p, size_type tableRow)`:

- `ParameterLength(p, tableRow)`: if `p == TheMmmInputImagesParameter` (table itself, `tableRow`
  ignored) → return `p_viewIds.Length()` (row count). If `p ==
  TheMmmImageViewIdParameter` → return `p_viewIds[tableRow].Length()` (that row's UTF-16
  character length).
- `AllocateParameter(n, p, tableRow)`: doc at `ProcessImplementation.h:698-700` — "For table
  parameters, `sizeOrLength` is a row count. For a string parameter, `sizeOrLength` is a string
  length in characters." If `p` is the table → resize `p_viewIds` to `n` rows. If `p` is the
  string column → resize/reserve `p_viewIds[tableRow]` to `n` characters.
- `LockParameter(p, tableRow)`: for the string column, return `p_viewIds[tableRow].Begin()` — a
  raw pointer the core reads/writes UTF-16 through directly until `UnlockParameter` (only called
  if `MetaString::NeedsUnlocking()` were overridden true — default false, so likely not needed
  for a plain string column).

This is registered automatically: `MetaParameter::PerformAPIDefinitions()`
(`MetaParameter.cpp:171-183`) registers `SetParameterLockRoutine` unconditionally, and — for any
`IsVariableLength()` parameter (string, table, *or* block) — also `SetParameterAllocationRoutine`
and `SetParameterLengthQueryRoutine`. So the same `AllocateParameter`/`ParameterLength` overrides
in your `ProcessImplementation` must dispatch on `p`'s identity to handle **both** the
table-row-count case and the string-column-length case.

### Summary gotchas to flag to the plan author

1. `MetaTable::Length()`/`operator[]` give column count/columns, **not** row count.
2. Nested tables are actively (runtime-)forbidden.
3. A table row's string column is an ordinary `MetaString` constructed with a `MetaTable*` parent.
4. Booleans/enums **must** back onto `pcl_bool`/`pcl_enum` (both int32-based) — not `bool`/a raw
   C++ `enum class` — per explicit ABI-compatibility warnings in the headers.
5. The five `ProcessImplementation` parameter hooks dispatch by **pointer identity** of the
   `MetaParameter*` singleton, not by name/ID string — expect an `if (p == TheXxxParameter) ...`
   chain. The convention of naming these globals `TheXxxParameter` is standard PCL SDK style but
   is not itself shown/enforced in `MetaParameter.h`.

---

## 4. ProcessInterface + GUI controls

Files read: `pcl/ProcessInterface.h` (2662), `pcl/ViewList.h`, `pcl/MultiViewSelectionDialog.h`
(114, read in full), `pcl/ViewSelectionDialog.h` (117, read in full), `pcl/NumericControl.h`,
`pcl/ComboBox.h`, `pcl/CheckBox.h` (97, read in full), `pcl/PushButton.h` (104, read in full),
`pcl/Button.h`, `pcl/Sizer.h` (612, read in full), `pcl/FileDialog.h` (503, read in full).

### ProcessInterface subclassing

`class ProcessInterface : public Control, public MetaObject` (`ProcessInterface.h:223`).

Pure virtuals:
```cpp
virtual IsoString Id() const = 0;             // 256
virtual MetaProcess* Process() const = 0;     // 293 — associated process
```

Key overridables:
```cpp
virtual InterfaceFeatures Features() const { return InterfaceFeature::Default; }  // 530
virtual void ApplyInstance() const;                                               // 547
virtual void ApplyInstanceGlobal() const;                                         // 561
virtual void ResetInstance() {}                                                    // 707
virtual bool Launch( const MetaProcess&, const ProcessImplementation* instance,
                     bool& dynamic, unsigned& flags ) { dynamic = false; return true; } // 808-813
bool Launch( unsigned flags = 0 );                                                 // 839, non-virtual convenience
virtual ProcessImplementation* NewProcess() const { return nullptr; }              // 879 — MUST reimplement for a real process
virtual bool IsInstanceGenerator() const { return true; }                          // 944 — false if NewProcess() not reimplemented
virtual bool ImportProcess( const ProcessImplementation& ) { return false; }       // 1060 — MUST reimplement
virtual bool CanImportInstances() const { return true; }                          // 1075
static void ProcessEvents( bool excludeUserInputEvents = false );                  // 2634
```
`Launch()` (deferred init) is where you build the child controls the first time the interface is
shown — `Initialize()` (733) is deprecated in favor of this. A plain "tool" interface with no
instance-generating capability only needs `Id()`, `Process()`, and `IsInstanceGenerator()`
overridden to `false` (doc note 870-875).

### ViewList — single-view selector

`class ViewList : public Control` (`ViewList.h:44`):
```cpp
ViewList( Control& parent = Control::Null() );                                   // 51
void Regenerate( bool mainViews = true, bool previews = true, bool realTimePreview = false ); // 157
View CurrentView() const;                                                         // 190
void SelectView( const View& view );                                              // 204 (View::Null() = "No View Selected")
void ExcludeView( const View& v ); View ExcludedView() const;                      // 174-182
using view_event_handler = void (Control::*)( ViewList& sender, View& view );      // 242
void OnViewSelected( view_event_handler handler, Control& receiver );              // 255
void OnCurrentViewUpdated( view_event_handler handler, Control& receiver );        // 269
```

### MultiViewSelectionDialog — the real multi-view-selection mechanism (use this, don't build one)

`pcl/MultiViewSelectionDialog.h`, 114 lines, read in full:
```cpp
class MultiViewSelectionDialog : public Dialog
{
public:
   MultiViewSelectionDialog( bool allowPreviews = true );          // 57
   const Array<View>& Views() const { return m_selectedViews; }    // 69 — result accessor
   // internally: VerticalSizer > TreeBox (checkable nodes) + Select All/Unselect All/
   // Include Main Views/Include Previews + OK/Cancel PushButtons
};
```
Invocation (modal, per `src/pcl/MultiViewSelectionDialog.cpp` OK/Cancel handlers calling
`Ok()`/`Cancel()`):
```cpp
MultiViewSelectionDialog d;
if ( d.Execute() == StdDialogCode::Ok )
   for ( const View& v : d.Views() )
      // use v
```
*(Not independently re-confirmed against `Dialog.h` in this pass — `Execute()`'s exact return
type/`StdDialogCode` enum should get a one-line confirmation read before the plan locks it in.)*

Internally populates from `View::AllViews()` and resolves checked nodes back via
`View::ViewById(node->Text(0))` — this is the standard, ready-made way to let the user pick
several input views for mmm-pxm; no custom multi-select widget is needed.

### NumericControl / NumericEdit

`pcl/NumericControl.h`:
```cpp
class NumericEdit : public Control {                                    // 46
   NumericEdit( Control& parent = Null() );                             // 57
   double Value() const;  void SetValue( double );                      // 69, 77
   virtual void SetRange( double lower, double upper );                 // 140
   void SetPrecision( int n );                                          // 151
   void SetReal( bool = true );  void SetInteger( bool = true );        // 107-114
   using value_event_handler = void (Control::*)( NumericEdit& sender, double value ); // 292
   void OnValueUpdated( value_event_handler, Control& );                // 296
};
class NumericControl : public NumericEdit {   // adds a slider — typical for module UIs
   NumericControl( Control& parent = Null() ); // 357
   void SetRange( double lower, double upper ) override; // 379
   void EnableExponentialResponse( bool = true );         // 429
};
```

### ComboBox

`pcl/ComboBox.h`, `class ComboBox : public Control` (44):
```cpp
ComboBox( Control& parent = Control::Null() );                              // 51
int CurrentItem() const;  void SetCurrentItem( int index );                 // 69, 74
void AddItem( const String& text, const Bitmap& icon = Bitmap::Null() );    // 112
using item_event_handler = void (Control::*)( ComboBox& sender, int itemIndex ); // 449
void OnItemSelected( item_event_handler, Control& receiver );                // 476
```

### CheckBox / PushButton / Button event typedefs

`pcl/CheckBox.h` (97, read in full), `class CheckBox : public Button` (42):
```cpp
CheckBox( const String& text = String(), Control& parent = Control::Null() );  // 50
bool IsCheckable() const override { return true; }                             // 70
```
`pcl/PushButton.h` (104, read in full), `class PushButton : public Button` (42):
```cpp
PushButton( const String& text = String(), const Bitmap& icon = Bitmap::Null(),
            Control& parent = Control::Null() );                               // 50
bool IsPushable() const override { return true; }                              // 64
```
`SetText()`, checked-state, and click/press/check events inherited from `Button`
(`pcl/Button.h:71`):
```cpp
String Text() const;  void SetText( const String& );                    // 91, 96
bool IsChecked() const;  void SetChecked( bool = true );                 // 226, 231
using click_event_handler = void (Control::*)( Button& sender, bool checked ); // 285
using press_event_handler = void (Control::*)( Button& sender );              // 297
using check_event_handler = void (Control::*)( Button& sender, Button::check_state ); // 311
void OnClick( click_event_handler, Control& receiver );                  // 324
void OnPress/OnRelease/OnCheck( ... );                                    // 337, 350, 363
```

### FileDialog family (there IS a directory picker)

`pcl/FileDialog.h`, 503 lines, read in full. Abstract base `FileDialog` (161) with
`virtual bool Execute() = 0;` (272). Three concrete classes:
```cpp
class OpenFileDialog : public FileDialog {           // 289
   OpenFileDialog();
   void LoadImageFilters();                          // 317, auto-populate from installed format modules
   void EnableMultipleSelections( bool = true );      // 335
   bool Execute() override;                           // 352
   const StringList& FileNames() const;               // 362
   String FileName() const;                           // 371
};
class SaveFileDialog : public FileDialog { ... };      // 388, mirrors OpenFileDialog
class GetDirectoryDialog : public FileDialog {         // 468 — THE directory picker
   GetDirectoryDialog();
   bool Execute() override;                            // 484
   String Directory() const;                           // 489
};
```
```cpp
GetDirectoryDialog d;
d.SetCaption( "Select Output Directory" );
if ( d.Execute() )
   String dir = d.Directory();
```

### Sizer / HorizontalSizer / VerticalSizer

`pcl/Sizer.h`, 612 lines, read in full:
```cpp
class Sizer : public UIObject {
   Sizer( bool vertical );                                                          // 129
   void Add( Sizer& s, int stretchFactor = 0 );                                     // 223
   void Add( Control& c, int stretchFactor = 0, item_alignment align = Align::Default ); // 260
   void AddSpacing( int size, bool autoScaling = true );                            // 273
   void AddStretch( int stretchFactor = 100 );                                      // 305
   void SetMargin( int margin, bool autoScaling = true );                          // 462
   void SetSpacing( int spacing, bool autoScaling = true );                        // 488
};
class HorizontalSizer : public Sizer { HorizontalSizer() : Sizer(false) {} };        // 547
class VerticalSizer   : public Sizer { VerticalSizer()   : Sizer(true)  {} };        // 579
```

### Event-wiring idiom — no `__CLASS_HANDLER` macro; confirmed by grep (zero matches)

The only related macro found is an internal guard, not a wiring helper (`Control.h:1824`):
```cpp
#define __PCL_NO_ALIAS_HANDLERS \
   if ( IsAlias() ) throw Error( "Aliased controls cannot set event handlers." )
```
**The real idiom**, confirmed against real production source
(`src/pcl/MultiViewSelectionDialog.cpp`, `ViewSelectionDialog.cpp`): call the control's `OnXxx()`
setter directly with a C-style cast of a member-function pointer to the handler typedef, plus
`*this` as receiver:
```cpp
SelectAll_PushButton.OnClick( (Button::click_event_handler)&MultiViewSelectionDialog::ButtonClick, *this );
IncludeMainViews_CheckBox.OnClick( (Button::click_event_handler)&MultiViewSelectionDialog::OptionClick, *this );
Images_ViewList.OnViewSelected( (ViewList::view_event_handler)&ViewSelectionDialog::ViewSelected, *this );
OnShow( (Control::event_handler)&MultiViewSelectionDialog::ControlShow, *this );
```
Generalized for a custom interface:
```cpp
myPushButton.OnClick( (Button::click_event_handler)&MyInterface::e_Click, *this );
myComboBox.OnItemSelected( (ComboBox::item_event_handler)&MyInterface::e_ItemSelected, *this );
myNumericControl.OnValueUpdated( (NumericControl::value_event_handler)&MyInterface::e_ValueUpdated, *this );
myViewList.OnViewSelected( (ViewList::view_event_handler)&MyInterface::e_ViewSelected, *this );
```
The member function's signature must match the raw typedef exactly (e.g. `void e_Click( Button&
sender, bool checked );`); the cast is needed because the handler is a member of your derived
class, not of `Control`.

### Progress indicator — no embeddable widget; use StatusMonitor + a Label, or a modal dialog

No embeddable "ProgressBar" control class exists (only `ProgressBarStatus.h`/`ProgressDialog.h`,
both modal-dialog based). Options:
- `pcl::ProgressBarStatus : public StatusCallback` (`ProgressBarStatus.h:53`) — ready-made modal
  progress dialog with Cancel; construct `ProgressBarStatus( const String& title, Control& parent
  = Control::Null() )` (66), attach via `monitor.SetCallback(&progressBarStatus)`
  (`StatusMonitor.h:467`).
- For non-modal, in-interface feedback: a plain `Label` updated from your own `StatusCallback`
  reimplementation, combined with periodic `ProcessInterface::ProcessEvents()` calls to keep the
  GUI responsive.

---

## 5. Enumerating views + reading ImageVariant pixel rows

Files read: `pcl/ImageWindow.h`, `pcl/View.h`, `pcl/ImageVariant.h`, `pcl/Image.h`,
`pcl/AbstractImage.h`, `pcl/ImageGeometry.h`.

### Enumerate all windows

```cpp
static Array<ImageWindow> AllWindows( bool includeIconicWindows = true );  // ImageWindow.h:3059
```
"Returns a container with all existing image windows, including visible and hidden windows, and
optionally iconized windows." Also `ActiveWindow()` (3050), `WindowById()` (3026),
`WindowByFilePath()` (3038).
```cpp
View MainView() const;   // ImageWindow.h:665 — "There is only one main view in an image window."
View CurrentView() const; // 674
```

### View class (`pcl/View.h`)

```cpp
bool IsMainView() const;                       // 248
ImageWindow Window() const;                    // 287 — parent window
IsoString Id() const;                          // 297 — unique within naming context
IsoString FullId() const;                      // 315 — "<image_id>-><id>" for previews
ImageVariant Image() const;                    // 492
void Lock( bool notify = true ) const;         // ~366 — MUST call before Image()
void Unlock( bool notify = true ) const;
void LockForWrite( bool notify = true ) const;
```
Doc for `Image()` (487-490): "Before calling this function... you must make sure that your
processing thread has the appropriate access rights to the view... done by calling the `Lock()`
member function." **This is a hard requirement, not optional** — PixInsight is multithreaded and
failing to lock risks corruption of the shared image the core app also touches.

`Image()` returns an `ImageVariant` by value that transports a *shared image* — "a managed alias
for an actual image living in the core PixInsight application" — so reads/writes through it act
on the real window pixels, no private copy.

### ImageVariant — type-erased container over sample formats

Format queries (`ImageVariant.h`):
```cpp
bool IsFloatSample() const noexcept;      // 409
bool IsComplexSample() const noexcept;    // 417
int  BitsPerSample() const noexcept;      // 426
int  BytesPerSample() const noexcept;     // 435 (bitsPerSample >> 3)
int  Width() const; int Height() const; int NumberOfChannels() const; // 444, 453, 473
color_space ColorSpace() const noexcept;  // 1432
bool IsSharedImage() const;               // 3071
```
Canonical resolution idiom, quoted from the class doc (173-193):
```cpp
ImageVariant image = view.Image();
if ( image )
{
   if ( image.IsComplexSample() )
      throw Error( "Complex images are not supported" );
   if ( image.IsFloatSample() )
      switch ( image.BitsPerSample() )
      {
      case 32 : DoSomething( static_cast<Image&>( *image ) ); break;  // pcl::Image = Float32
      case 64 : DoSomething( static_cast<DImage&>( *image ) ); break; // Float64
      }
   else
      switch ( image.BitsPerSample() )
      {
      case  8 : DoSomething( static_cast<UInt8Image&>( *image ) ); break;
      case 16 : DoSomething( static_cast<UInt16Image&>( *image ) ); break;
      case 32 : DoSomething( static_cast<UInt32Image&>( *image ) ); break;
      }
}
```
Type-erased bulk pixel-plane access **without** branching on sample type:
```cpp
void*       PixelData( int channel = 0 );          // 7105
const void* PixelData( int channel = 0 ) const noexcept; // 7126
```
Format-converting bulk copy: `CopyImage(const GenericImage<P>&)` / `CopyImage(const
ImageVariant&)` (6598-6644) — "PCL performs the assignment between different image types
transparently." Useful to normalize a non-Float32 view in one call:
```cpp
ImageVariant tmp; tmp.CreateFloatImage(); tmp.CopyImage( view.Image() );
```

### `pcl::Image` = Float32 (`Image.h:18398,18467`)

```cpp
using FImage = GenericImage<FloatPixelTraits>;   // 18398
using Image  = FImage;                           // 18467
```
Other typedefs: `DImage` (64-bit float), `UInt8Image`/`UInt16Image`/`UInt32Image`,
`FComplexImage`/`DComplexImage`.

Pixel access on `GenericImage<P>` (6668-6845):
```cpp
sample* PixelData( int channel = 0 );                     // 6682, MUTABLE — calls EnsureUnique() first
const sample* PixelData( int channel = 0 ) const noexcept; // 6695, read-only, no copy trigger
sample* ScanLine( int y, int channel = 0 );                     // 6781, mutable — EnsureUnique()
const sample* ScanLine( int y, int channel = 0 ) const noexcept;// 6795, read-only
```
**Copy-on-write warning** (6676-6680, 6777-6779): mutable overloads call `EnsureUnique()`, which
duplicates the pixel data if shared/aliased. **For a read-only bulk copy into shared memory, use
the `const` overloads** to avoid an unwanted duplication of a potentially multi-GB shared image.

`ChannelSize()` (`ImageVariant.h:1358`) = `BytesPerSample() * Width() * Height()` — use this to
size the shared-memory buffer per channel.

### Storage layout: confirmed PLANAR (channel-separated), not interleaved

Definitive comment, `Image.h:15463-15479`:
```cpp
struct Data : public ReferenceCounter
{
   // Each element in the data array points to a contiguous block of pixel
   // samples that stores one channel of the image. This includes all
   // nominal and alpha channels.
   sample** data = nullptr;
};
```
and `Image.h:192`:
```cpp
#define m_channelData( c ) reinterpret_cast<sample*>( PCL_ASSUME_ALIGNED_32( m_pixelData[c] ) )
```
i.e. one pointer per channel, each a 32-byte-aligned contiguous plane of `Width()*Height()`
samples — **no per-pixel interleaving**. A whole channel plane can be `memcpy`'d in one shot via
`ScanLine(0, channel)` or `PixelData(channel)` for `ChannelSize()` bytes — no pixel-by-pixel loop
needed.

### No dedicated bulk "ReadPixels" beyond per-channel planes

`GetLuminance()`/`GetIntensity()` are colorimetric grayscale-derivation operators (RGB→single
channel), not raw bulk copiers. The efficient path for mmm-pxm is: for each channel `c` in
`0..NumberOfChannels()`, `const sample* p = view.Image().PixelData(c)` (or resolve to
`pcl::Image` and use `ScanLine`) and `memcpy` `ChannelSize()` bytes into shared memory.

### Uncertainty flagged

No guarantee in these headers that a `MosaicByCoordinates`-produced view is always Float32 — that
is a PixInsight-workflow assumption external to PCL. mmm-pxm should gate on
`IsFloatSample()`/`IsComplexSample()`/`BitsPerSample()` and either reject non-Float32 views with a
clear error, or normalize via `CopyImage()` first.

---

## 6. Astrometric solution detection + raw property extraction (trickiest area)

Files read: `pcl/AstrometricMetadata.h` (full, 1512 lines), `pcl/View.h` (property section),
`pcl/ImageWindow.h` (astrometric section), `pcl/Property.h` (full), `pcl/PropertyDescription.h`
(full), `pcl/Variant.h` (relevant sections), `src/pcl/AstrometricMetadata.cpp` (Build/constructor
sections), `pcl/WorldTransformation.h` (property-prefix constant). Also cross-checked the bundled
`pjsr/astrometry/AstrometricMetadata.js` reference script for corroboration (JS, not C++, but
same vendor/convention).

### Presence check

```cpp
bool HasAstrometricSolution() const;   // ImageWindow.h:1343
```
"Returns true iff this image window has a valid astrometric solution." Backed by a real core API
function (`GetImageWindowHasAstrometricSolution`) — **this is the cheap, authoritative check**,
cheaper than constructing an `AstrometricMetadata` and calling `IsValid()`. *(Its internal
implementation lives in the core application, not in the shipped PCL sources — behavior
confirmed only by the public header doc, not traced byte-for-byte.)*

Fallback / equivalent from a `View`: construct `AstrometricMetadata` from the window and check
validity:
```cpp
AstrometricMetadata( const ImageWindow& window );   // AstrometricMetadata.h:183
bool IsValid() const { return !m_projection.IsNull() && !m_transformWI.IsNull(); } // 236-239
```
Constructor body (`src/pcl/AstrometricMetadata.cpp:158-165`):
```cpp
AstrometricMetadata::AstrometricMetadata( const ImageWindow& window )
{
   int width, height;
   View view = window.MainView();
   view.GetSize( width, height );
   Build( view.Properties(), window.Keywords(), width, height );
}
```
So validity is *derived* by attempting to build a WCS/projection from the view's raw properties +
FITS keywords — there's no single boolean property flag being checked.

### `AstrometricMetadata` is mostly DERIVED accessors, not raw passthrough — but has two real bridges

The bulk of its public API (`ImageToCelestial()`, `CelestialToImage()`, `Resolution()`,
`Rotation()`, `Projection()`, `WorldTransform()`, `ReferenceSystem()`, `PixelSize()`, `Catalog()`,
`CreatorApplication()`, ...) is computed/typed getters, **not** raw key/value pairs. For raw
passthrough, use these two members instead:

```cpp
PropertyArray ToProperties() const;   // AstrometricMetadata.h:1064 — serializes the solution AS XISF properties
void Build( const PropertyArray& properties, const FITSKeywordArray& keywords,
            int width, int height, bool regenerate = false );   // 1016 — the inverse
void UpdateProperties( PropertyArray& properties ) const;         // 1200 — patch an existing array in place
```
**Conclusion for mmm-pxm**: if you already have the raw `PropertyArray` from `view.Properties()`,
you do **not** need `AstrometricMetadata` at all to forward it — filter by id prefix and pass the
`Property` objects straight through (see below). `AstrometricMetadata` is only needed if you must
recompute/re-derive a solution, or regenerate a canonical serialization via `ToProperties()`.

### The exact `PCL:AstrometricSolution:*` property id list

Confirmed from three duplicated doc-comment blocks in `AstrometricMetadata.h` (1019-1062,
959-996, 1144-1198), cross-checked against literal string usage in `AstrometricMetadata.cpp`:

Standard XISF properties (not `PCL:`-prefixed):
```
Instrument:Sensor:XPixelSize
Instrument:Sensor:YPixelSize
Instrument:Telescope:FocalLength
Observation:CelestialReferenceSystem
Observation:Center:Dec
Observation:Center:RA
Observation:Equinox
Observation:Location:Elevation
Observation:Location:Latitude
Observation:Location:Longitude
Observation:Time:End
Observation:Time:Start
```
Nonstandard PCL native-solution properties (core ≥ 1.8.9-2):
```
PCL:AstrometricSolution:Catalog
PCL:AstrometricSolution:CelestialPoleNativeCoordinates
PCL:AstrometricSolution:CreationTime
PCL:AstrometricSolution:CreatorApplication
PCL:AstrometricSolution:CreatorModule
PCL:AstrometricSolution:CreatorOS
PCL:AstrometricSolution:Information
PCL:AstrometricSolution:LinearTransformationMatrix
PCL:AstrometricSolution:ProjectionSystem
PCL:AstrometricSolution:ReferenceCelestialCoordinates
PCL:AstrometricSolution:ReferenceImageCoordinates
PCL:AstrometricSolution:ReferenceNativeCoordinates
```
Plus, for spline-based (non-linear) solutions, everything under prefix
`PCL:AstrometricSolution:SplineWorldTransformation:` — confirmed literal (`WorldTransformation.h:603-606`):
```cpp
static IsoString PropertyPrefix() { return "PCL:AstrometricSolution:SplineWorldTransformation:"; }
```
One legacy exception, checked explicitly in `Build()` (`AstrometricMetadata.cpp:92`): the single
property `PCL:AstrometricSolution:SplineWorldTransformation` (no trailing colon/suffix) is a
core-1.8.9-2-only monolithic blob — handle as a special case alongside the prefixed set if
filtering by prefix.

### Prefix-filtering is the sanctioned pattern (confirmed by PCL's own code)

`Property::IsValidIdentifier()` tokenizes ids on `:` (`Property.h:174-184`). PCL's own `Build()`
filters by literal prefix (`src/pcl/AstrometricMetadata.cpp:90-92`):
```cpp
const IsoString splineWTPrefix = SplineWorldTransformation::PropertyPrefix();
for ( const Property& property : properties )
   if ( property.Id().StartsWith( splineWTPrefix )
     || property.Id() == "PCL:AstrometricSolution:SplineWorldTransformation"
     || property.Id() == "Transformation_ImageToProjection" )  // core < 1.8.9-2
      hasSplineWT = true;
```
So `IsoString::StartsWith("PCL:AstrometricSolution:")` against `view.Properties()` ids is a
legitimate, precedented filter. Note `View.h:607-611` documents a **separate**, reserved
`"PixInsight:"` prefix namespace that modules must never write to — unrelated to `PCL:`.

### View property API (exact signatures, `pcl/View.h`)

```cpp
PropertyDescriptionArray PropertyDescriptions() const;   // 627 — id + type only
PropertyArray Properties() const;                        // 635 — all readable (id, value) properties
PropertyArray StorableProperties() const;                 // 647
PropertyArray PermanentProperties() const;                 // 659
PropertyArray StorablePermanentProperties() const;          // 674
Variant PropertyValue( const IsoString& property ) const;   // 776 — invalid Variant if undefined;
                                                              //   throws Error if read-protected
bool HasProperty( const IsoString& property ) const;         // 1002
void SetPropertyValue( const IsoString& property, const Variant& value,
                       bool notify = true,
                       ViewPropertyAttributes attributes = ViewPropertyAttribute::NoChange ); // 864
void SetProperties( const PropertyArray& properties, bool notify = true,
                    ViewPropertyAttributes attributes = ViewPropertyAttribute::NoChange );     // 706
                    // doc gives: view2.SetProperties( view1.Properties() ); — directly usable
                    // for bulk property-copy on the output window (minus filtering)
Variant::data_type PropertyType( const IsoString& property ) const;    // 949
ViewPropertyAttributes PropertyAttributes( const IsoString& property ) const; // 968
static bool IsReservedViewPropertyId( const IsoString& id );             // 616
```

### `Property` / `PropertyArray` / `PropertyDescription` (`pcl/Property.h`, `pcl/PropertyDescription.h`)

```cpp
class Property
{
public:
   using identifier_type = IsoString;
   using value_type = Variant;
   Property( const identifier_type& identifier, const value_type& value );
   const identifier_type& Identifier() const;   // == Id()
   const value_type& Value() const;
   void SetValue( const value_type& value );
   data_type Type() const;
   static bool IsValidIdentifier( const IsoString& id );
   bool IsValid() const;
};
using PropertyArray = Array<Property>;

struct PropertyDescription
{
   IsoString id;
   VariantType::value_type type = VariantType::Invalid;
};
using PropertyDescriptionArray = Array<PropertyDescription>;
```
`Property` is literally an id+`Variant` pair — the natural raw-passthrough vehicle: iterate
`view.Properties()`, filter `p.Id().StartsWith("PCL:AstrometricSolution:")` (plus the standard
`Observation:*`/`Instrument:*` ids if the helper wants pixel-scale/observation metadata too), and
serialize `p.Id()` + `p.Value()`.

### `Variant` value types relevant to metadata (`pcl/Variant.h`)

`VariantType::value_type` includes `Bool`, integer/float scalars, `TimePoint`, vector types
(`DVector`/`F64Vector` etc.), matrix types (`DMatrix`/`F64Matrix`), `String`/`IsoString`,
key-value(-list) variants. The astrometric properties above use `IsoString`/`String` (Catalog,
CreatorApplication/Module/OS, ProjectionSystem), `DVector` (reference coordinates), `DMatrix`
(LinearTransformationMatrix), `TimePoint` (CreationTime), `Int32`/`Float32`/`Bool` (spline
parameters).
```cpp
bool IsValid() const;                 // 984
data_type Type() const;               // 992
bool ToBool() const;                  // 1065
double ToDouble() const;              // 1171
TimePoint ToTimePoint() const;        // 1232
Vector ToVector() const;              // 1486
Matrix ToMatrix() const;              // 1684
String ToString() const;              // 1751
IsoString ToIsoString() const;        // 1766
```
Each has a paired `CanConvertToX() const noexcept` guard. PCL's own internal code (e.g.
`AstrometricMetadata.cpp:96,98`) calls `property.Value().ToTimePoint()`/`.ToString()` directly
without checking `Type()` first — your module forwarding to an external helper should probably be
more defensive (check type before converting).

### Confirmed vs uncertain

**Confirmed**: `HasAstrometricSolution()` signature; `AstrometricMetadata`'s derived-vs-bridge
split (`ToProperties()`/`Build()`/`UpdateProperties()`); the exact property id list (triple-cross-
referenced); the spline prefix constant; all `View`/`Property`/`Variant` signatures above.

**Uncertain/inferred**:
- `HasAstrometricSolution()`'s internal implementation body is core-application code, not in the
  shipped PCL sources — only the `Build()`-based path (used by the public `AstrometricMetadata`
  constructor) was traced fully; in practice they should agree but this wasn't verified
  byte-for-byte.
- Whether `PCL:AstrometricSolution:Information` is actively populated or vestigial is unclear —
  it appears only in the doc-comment id lists, never referenced by name elsewhere in headers/src.
- The precise wire format/byte layout serialized under `SplineWorldTransformation:*` keys was not
  traced (would require reading `WorldTransformation.h`/`.cpp` in full) — if the helper needs to
  decode spline data rather than pass it through opaquely, that's a follow-up read.

---

## 7. Creating, filling, and showing the output ImageWindow

File: `pcl/ImageWindow.h`.

### Constructor

```cpp
ImageWindow( int width, int height, int numberOfChannels = 1,
             int bitsPerSample = 32, bool floatSample = true, bool color = false,
             bool initialProcessing = true,
             const IsoString& id = IsoString() );   // 357-360
```
Doc (312-356): `bitsPerSample` supports 8/16/32 for integer, 32/64 for float; pixels start zero
(black); "The new image window will be hidden. To make it visible... you must call its Show()
member function explicitly." `id` empty ⇒ auto-generated; non-unique ⇒ core appends a suffix.

For mmm-pxm's output: `ImageWindow( width, height, numberOfChannels, 32, true, /*color=*/numberOfChannels>=3, true, "MergeMosaic_output" )`.

### Getting the mutable image to write into

```cpp
View MainView() const;         // ImageWindow.h:665
ImageVariant Image() const;    // View.h:492 — shared alias into the real window pixels
```
Same locking requirement as reading (§5): `View::Lock()`/`LockForWrite()` before writing,
`Unlock()` after. Resolve to a concrete `pcl::Image&` (Float32) via the `IsFloatSample()` +
`BitsPerSample()` switch/`static_cast` idiom from §5, then use the **mutable**
`PixelData(channel)`/`ScanLine(y,channel)` overloads (which trigger `EnsureUnique()` — expected
and fine here since this is a brand-new, non-shared image you just created) to `memcpy` each
channel plane in from your blended result.

### Naming, showing, zooming

```cpp
IsoString View::Id() const;                          // View.h:297
void      View::Rename( const IsoString& newId );    // View.h:327 — set/rename after creation
void      Show( bool fitWindow = true );              // ImageWindow.h:2188 — also calls ZoomToFit() if true
void      ZoomToFit( bool optimalFit = true, bool allowMagnification = true,
                     bool allowAnimations = true, bool noLimits = false );  // 2024-2027
void      Hide();  bool IsVisible() const;  void BringToFront();          // 2194, 2171, 2220
```
There is no separate "window filename" setter distinct from the view id — `ImageWindow` itself
has no `SetId`, only the generic `UIObject::SetObjectId(const String&)`
(`pcl/UIObject.h:243`) and static lookup `ImageWindow::WindowById(const IsoString&)` (3026).

### Minimal skeleton

```cpp
pcl::ImageWindow w( outW, outH, outChannels, 32, /*floatSample*/true,
                     /*color*/outChannels >= 3, /*initialProcessing*/true, "MergeMosaic" );
pcl::View v = w.MainView();
v.LockForWrite();
pcl::ImageVariant iv = v.Image();
// iv resolved to pcl::Image& per §5's IsFloatSample()/BitsPerSample() idiom
for ( int c = 0; c < outChannels; ++c )
   ::memcpy( image.PixelData( c ), sourceChannelBuffer[c], image.ChannelSize() );
v.UnlockForWrite();
w.Show();
```

---

## 8. Module's own on-disk file path (to find the sibling worker binary)

Files checked: `pcl/MetaModule.h` (full), `pcl/GlobalSettings.h` (full grep for
directory/path-related settings). There is no separate `Module.h` (`ls pcl/ | grep -i module` →
only `MetaModule.h`).

**Confirmed: no PCL API exposes the module's own loaded `.so` path.**

- `extern MetaModule* Module;` (`MetaModule.h:690`) is the well-known global reachable everywhere
  in your module's code, but `MetaModule` has no `FilePath()`/`Directory()` method.
- `MetaModule::OriginalFileName()` (306) is a purely informational, developer-supplied virtual
  (defaults to empty string) — **not** filesystem-derived; it's whatever string you choose to
  return, not queried from disk.
- `GlobalSettings.h` (`PixInsightSettings`) exposes many `Application/*Directory`/`*FilePath` keys
  (e.g. `Application/BinDirectory`, `Application/CoreFilePath`, `Application/RscDirectory`,
  lines 281-327) — these describe the **core distribution's own** directories, not the calling
  module's file. No `ModulesDirectory` or per-module path key exists anywhere in this header.
- `dladdr` is not referenced anywhere under `/opt/PixInsight/include/pcl/*.h` — PCL does not wrap
  it.

**Conclusion**: the fallback you proposed is correct and is the only option — call POSIX
`dladdr()` against the address of any function/symbol defined inside `mmm-pxm.so` (e.g. a static
helper function's address, or the `InstallPixInsightModule` symbol itself) to get
`Dl_info::dli_fname`, the absolute path of the shared object containing that address, then derive
the sibling worker binary's path from its directory.

```cpp
#include <dlfcn.h>
static pcl::String ModuleDirectory()
{
   Dl_info info;
   if ( dladdr( reinterpret_cast<void*>( &InstallPixInsightModule ), &info ) && info.dli_fname )
      return pcl::File::ExtractDrive( info.dli_fname ) + pcl::File::ExtractDirectory( info.dli_fname );
   throw pcl::Error( "mmm-pxm: could not determine module path via dladdr()" );
}
```
*(exact `pcl::File` helper names for path splitting should be double-checked against `pcl/File.h`
when writing this for real — not verified in this pass.)*

---

## 9. Long-running work: threading, progress, and abort

Files read in full: `pcl/StatusMonitor.h` (591 lines), plus targeted sections of
`pcl/StandardStatus.h` + `src/pcl/StandardStatus.cpp`, `pcl/Thread.h`, `pcl/Console.h`,
`pcl/Exception.h`.

### StatusMonitor / StatusCallback — progress + abort signaling

`StatusCallback` (abstract, `StatusMonitor.h:65-159`) — pure virtuals your callback implements:
```cpp
virtual int  Initialized( const StatusMonitor& monitor ) const = 0;  // 113
virtual int  Updated( const StatusMonitor& monitor ) const = 0;       // 130
virtual int  Completed( const StatusMonitor& monitor ) const = 0;     // 149
virtual void InfoUpdated( const StatusMonitor& monitor ) const = 0;   // 158
```
Each of `Initialized`/`Updated`/`Completed` "must return zero if the process can continue. If
this function returns a nonzero value, the ... process is aborted **by throwing a
`ProcessAborted` exception**." (`ProcessAborted` declared `pcl/Exception.h:673`:
`PCL_DECLARE_EXCEPTION_CLASS( ProcessAborted, "Process aborted", String() );`)

`StatusMonitor` (164-591) — what you drive from your worker code:
```cpp
void Initialize( const String& info, size_type count = 0 );  // 244; count=0 ⇒ "unbounded" monitor
void operator ++();                                            // 402 — advance by 1, dispatches callback
void operator +=( size_type n );                                // 414 — advance by n
void Complete();                                                // 431 — force completion (needed if unbounded)
bool IsAborted() const;                                          // 370 — post-hoc check
void SetCallback( StatusCallback* callback );                    // 467 (nullptr = detach)
```
"%StatusMonitor utilizes a low-priority timing thread to generate callback monitoring calls
asynchronously at constant time intervals" (183-186); default refresh 250ms
(`SetRefreshRate()`/`RefreshRate()`, 25-999ms range, 537-557).

### StandardStatus — the concrete console-attached callback

`pcl/StandardStatus.h` + real implementation `src/pcl/StandardStatus.cpp`:
```cpp
int Initialized( const StatusMonitor& ) const override;  // 134
int Updated( const StatusMonitor& ) const override;       // 140
int Completed( const StatusMonitor& ) const override;      // 146
void InfoUpdated( const StatusMonitor& ) const override;    // 152
```
Real `Updated()` body shows the wiring pattern (abort detection + console percentage output):
```cpp
int StandardStatus::Updated( const StatusMonitor& monitor ) const
{
   if ( m_thread != 0 )
   {
      if ( ThreadAborted( m_thread ) ) { m_console.WriteLn( "<end>*" ); return 1; }
   }
   else
   {
      if ( m_console.AbortRequested() ) { m_console.Abort(); return 1; }
      int percent = pcl::Range( pcl::RoundInt( 100*(double(monitor.Count())/monitor.Total()) ), 0, 100 );
      // ... writes percentage to console ...
   }
   return 0;
}
```
Usage: `pcl::StandardStatus status; monitor.SetCallback( &status );` then drive `monitor` as
above — automatic console percentage + abort detection.

### pcl::Thread — subclass, Run(), Start()/Wait(), Abort()

`pcl/Thread.h`:
```cpp
class Thread : public UIObject { ... };                                   // 200
virtual void Run();          // 496, default empty body; "Derived classes must reimplement"
void Start( priority = ThreadPriority::Inherit, int processor = -1 );      // 303
bool IsActive() const;                                                     // 395
void Wait();  bool Wait( unsigned ms );                                    // 425, 442
void Abort() { SetStatus( 0x80000000 ); }                                  // 576
bool IsAborted() const;  bool TryIsAborted() const;                        // 593, 609 (non-blocking)
static int NumberOfThreads(...);  static ThreadLoads OptimalThreadLoads(...); // 724, 775 — parallel work split
```
Abort doc (556-579, quoted): "If the thread calls `Module->ProcessEvents()` after an abort
message has been sent, or if it uses some of the standard status monitoring classes (such as
`StandardStatus` for example), a `ProcessAborted` exception will be thrown automatically in the
thread. The exception will be thrown in the (reimplemented) `Thread::Run()` member function,
where it should be caught and used to terminate thread execution by returning from `Run()`."

GUI-pump companion, `pcl::Module->ProcessEvents(bool excludeUserInputEvents = false)`
(`MetaModule.h:494-527`): "Call this function from the root thread... to let the PixInsight core
application process pending interface events... Modules typically call this function during
real-time preview generation procedures... calling this function at 250 ms intervals is
reasonable." When called from a running thread that has received `Abort()`, `ProcessAborted` is
thrown automatically (cross-referenced from `Thread::Abort()`'s own doc).

### Console — write, abort query, flush

`pcl/Console.h`:
```cpp
void Write( const String& s );    void WriteLn( const String& s );  void WriteLn();  // 381, 390, 398
void Flush();                                                                          // 585
bool AbortEnabled() const;  bool AbortRequested() const;                               // 509, 515
void EnableAbort();  void DisableAbort();                                              // 543, 554
void Abort();  // "Accepts a pending abort request"                                    // 565
void ResetStatus();  // clears a pending abort (e.g. after user says "No" to a confirm) // 532
```
Abort methods are only meaningful "from the thread where either a reimplemented
`ProcessImplementation::ExecuteOn()` or `ExecuteGlobal()` member function has been invoked."

### Idiom for mmm-pxm's `ExecuteGlobal()` (synthesized — no worked example found in the tree)

```cpp
bool MmmBlendInstance::ExecuteGlobal()
{
   pcl::StandardStatus status;
   pcl::StatusMonitor monitor;
   monitor.SetCallback( &status );
   monitor.Initialize( "Blending panels", totalWorkUnits );

   // Spawn helper process, stream pixels over shared memory, poll for its progress...
   while ( helperStillWorking )
   {
      // update monitor as chunks complete:
      monitor += chunkWorkUnits;                 // throws ProcessAborted if the user aborted
      pcl::Module->ProcessEvents();              // keep GUI responsive; also surfaces Thread::Abort()
   }
   monitor.Complete();

   // ... build output ImageWindow per §7 ...
   return true;
}
```
Catch `pcl::ProcessAborted` around the body to clean up the helper subprocess and shared-memory
segment before letting the exception propagate (or return `false` after cleanup, per the
`bool ExecuteGlobal()` contract).

### Confirmed vs. inferred

**Confirmed**: all `StatusMonitor`/`StatusCallback`/`Thread`/`Console` signatures and doc-comment
quotes above, and the real `StandardStatus::Updated()` body.

**Inferred / flagged**: no bundled example module anywhere under `/opt/PixInsight/src` shows a
literal `ExecuteGlobal()` pumping `ProcessEvents()` in a wait loop while a helper subprocess runs
— the loop pattern above is assembled from the cross-referenced doc comments (`Thread::Abort()`
↔ `Module->ProcessEvents()` ↔ `ProcessAborted`), not copied from working code. The plan author
should treat this as the best-supported synthesis available locally, and verify against the
open-source PCL repo or a real module's source if in doubt before finalizing the plan.

---

## Overall risk summary

The single biggest uncertainty across this whole reference: **there is no complete example PCL
module anywhere in this install** — every skeleton above (module entry point wiring, the
`MetaTable`/view-id-list storage pattern, the `ExecuteGlobal()` progress/abort loop) is
synthesized from header doc comments and confirmed `.cpp` constructor/implementation bodies, not
copied from working code. The header comments are unusually thorough and the constructor/registration
mechanics were cross-checked directly against `.cpp` sources (not just doc prose), so confidence
is reasonably high — but the plan author should budget time to validate the module-registration
skeleton and the `MetaTable` row-storage dispatch against the open-source PCL repo
(gitlab.com/pixinsight/PCL) or any real third-party module source before writing the final
implementation, since these are the two areas with the most "assembled from convention" rather
than "copied from a working example" risk.
