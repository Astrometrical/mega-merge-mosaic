// MmmParameters.h -- MetaParameter singletons for MmmBlendProcess (Task 2).
//
// Each class below is a formal, per-process metadata description of one
// scriptable/serializable parameter of the MegaMergeMosaic blend process. They
// carry NO per-instance value: actual values live in MmmBlendInstance as
// ordinary C++ members and are exposed to the core through the five
// LockParameter/AllocateParameter/ParameterLength/... hooks, dispatched by
// pointer identity of the corresponding TheMmm...Parameter singleton.
//
// Signatures follow PCL_API_REFERENCE.md section 3, verified against
// /opt/PixInsight/include/pcl/MetaParameter.h (PCL 2.10.4):
//   - MetaTable / MetaString columns implement the view-id / file-path LISTS
//     (a MetaString column is a plain MetaString constructed with a MetaTable*
//     parent; the string's per-row storage is an Array<String> on the
//     instance).
//   - Booleans / enumerations MUST back onto pcl_bool / pcl_enum (both
//     int32-based) on the instance, NOT C++ bool / enum class.
//
// The parameter <-> wire mapping (spec 10.1, PROTOCOL.md 6 BlendParamsWire):
//   inputImages/viewId -> InitJob.panels (view ids)
//   filePaths/path     -> JobMode::Files.paths
//   inputSelect        -> JobMode (views) / InputSelect (files)
//   sessionDir         -> InitJob.session_dir
//   feather            -> params.feather_px
//   blendMode          -> params.mode ("feather"/"twoband"/"pyramid")
//   flatten(+enabled)  -> params.flatten (u32 or null)
//   (roi removed from UI; wire field is mandatory -> params.roi = null)
//   (downsample removed from UI; wire field is mandatory -> params.downsample = 1)
//   defectVeto         -> params.defect_veto
//   surfaceOrder       -> params.surface_order
//   bandRows           -> params.band_rows

#ifndef __MmmParameters_h
#define __MmmParameters_h

#include <pcl/MetaParameter.h>

namespace pcl
{

// ----------------------------------------------------------------------------
// inputImages: table of input view ids (>= 2 rows to blend), one MetaString
// column "viewId". Mutually exclusive with filePaths (host enforces).
// ----------------------------------------------------------------------------

/*!
 * \brief Table parameter holding the ordered list of input view ids.
 */
class MmmInputImagesParameter : public MetaTable
{
public:

   MmmInputImagesParameter( MetaProcess* P ) : MetaTable( P ) {}
   IsoString Id() const override { return "inputViews"; }
   size_type MinLength() const override { return 2; }   // need >= 2 views to blend
};

extern MmmInputImagesParameter* TheMmmInputImagesParameter;

/*!
 * \brief The "viewId" string column of the inputImages table.
 */
class MmmViewIdParameter : public MetaString
{
public:

   MmmViewIdParameter( MetaTable* T ) : MetaString( T ) {}   // parent is the TABLE
   IsoString Id() const override { return "viewId"; }
};

extern MmmViewIdParameter* TheMmmViewIdParameter;

// ----------------------------------------------------------------------------
// filePaths: table of input file paths, one MetaString column "path".
// Mutually exclusive with inputImages (host enforces).
// ----------------------------------------------------------------------------

/*!
 * \brief Table parameter holding the ordered list of input file paths.
 */
class MmmFilePathsParameter : public MetaTable
{
public:

   MmmFilePathsParameter( MetaProcess* P ) : MetaTable( P ) {}
   IsoString Id() const override { return "filePaths"; }
   size_type MinLength() const override { return 2; }   // need >= 2 files to blend
};

extern MmmFilePathsParameter* TheMmmFilePathsParameter;

/*!
 * \brief The "path" string column of the filePaths table.
 */
class MmmPathParameter : public MetaString
{
public:

   MmmPathParameter( MetaTable* T ) : MetaString( T ) {}   // parent is the TABLE
   IsoString Id() const override { return "path"; }
};

extern MmmPathParameter* TheMmmPathParameter;

// ----------------------------------------------------------------------------
// inputSelect: advanced Auto/Aligned/Solved override (default Auto). Element
// ORDER matters for Task 5 wire serialization; values equal indices.
// ----------------------------------------------------------------------------

/*!
 * \brief Enumeration: input interpretation override (Auto/Aligned/Solved).
 */
class MmmInputSelectParameter : public MetaEnumeration
{
public:

   enum { Auto, Aligned, Solved, NumberOfItems, Default = Auto };

   MmmInputSelectParameter( MetaProcess* P ) : MetaEnumeration( P ) {}
   IsoString Id() const override { return "inputSelect"; }
   size_type NumberOfElements() const override { return NumberOfItems; }
   IsoString ElementId( size_type i ) const override
   {
      switch ( i )
      {
      case Aligned: return "Aligned";
      case Solved:  return "Solved";
      default:      return "Auto";
      }
   }
   int ElementValue( size_type i ) const override { return int( i ); }
   size_type DefaultValueIndex() const override { return Default; }
};

extern MmmInputSelectParameter* TheMmmInputSelectParameter;

// ----------------------------------------------------------------------------
// blendMode: Feather/TwoBand/Pyramid (default Pyramid). Element order maps to
// wire strings "feather"/"twoband"/"pyramid" (Task 5 serializes the value).
// ----------------------------------------------------------------------------

/*!
 * \brief Enumeration: blend algorithm (Feather/TwoBand/Pyramid).
 */
class MmmBlendModeParameter : public MetaEnumeration
{
public:

   enum { Feather, TwoBand, Pyramid, NumberOfItems, Default = Pyramid };

   MmmBlendModeParameter( MetaProcess* P ) : MetaEnumeration( P ) {}
   IsoString Id() const override { return "blendMode"; }
   size_type NumberOfElements() const override { return NumberOfItems; }
   IsoString ElementId( size_type i ) const override
   {
      switch ( i )
      {
      case Feather: return "Feather";
      case TwoBand: return "TwoBand";
      default:      return "Pyramid";
      }
   }
   int ElementValue( size_type i ) const override { return int( i ); }
   size_type DefaultValueIndex() const override { return Default; }
};

extern MmmBlendModeParameter* TheMmmBlendModeParameter;

// ----------------------------------------------------------------------------
// sessionDir: user-owned session directory (InitJob.session_dir).
// ----------------------------------------------------------------------------

/*!
 * \brief String parameter: the *.mmm-session working directory.
 */
class MmmSessionDirParameter : public MetaString
{
public:

   MmmSessionDirParameter( MetaProcess* P ) : MetaString( P ) {}
   IsoString Id() const override { return "sessionDir"; }
};

extern MmmSessionDirParameter* TheMmmSessionDirParameter;

// ----------------------------------------------------------------------------
// feather: feather ramp length in canvas pixels (params.feather_px).
// ----------------------------------------------------------------------------

/*!
 * \brief Int32 parameter: feather ramp length in canvas pixels.
 */
class MmmFeatherParameter : public MetaInt32
{
public:

   MmmFeatherParameter( MetaProcess* P ) : MetaInt32( P ) {}
   IsoString Id() const override { return "feather"; }
   double DefaultValue() const override { return 256; }
   double MinimumValue() const override { return 1; }      // mmm-core rejects feather_px <= 0
   double MaximumValue() const override { return 1024; }
};

extern MmmFeatherParameter* TheMmmFeatherParameter;

// ----------------------------------------------------------------------------
// flatten (+ flattenEnabled): opt-in global background flatten polynomial
// order (params.flatten, u32 or null). flattenEnabled off => null.
// ----------------------------------------------------------------------------

/*!
 * \brief Int32 parameter: background-flatten polynomial order.
 */
class MmmFlattenParameter : public MetaInt32
{
public:

   MmmFlattenParameter( MetaProcess* P ) : MetaInt32( P ) {}
   IsoString Id() const override { return "flatten"; }
   double DefaultValue() const override { return 2; }
   double MinimumValue() const override { return 1; }
   double MaximumValue() const override { return 2; }
};

extern MmmFlattenParameter* TheMmmFlattenParameter;

/*!
 * \brief Boolean parameter: whether background flattening is enabled.
 */
class MmmFlattenEnabledParameter : public MetaBoolean
{
public:

   MmmFlattenEnabledParameter( MetaProcess* P ) : MetaBoolean( P ) {}
   IsoString Id() const override { return "flattenEnabled"; }
   bool DefaultValue() const override { return false; }
};

extern MmmFlattenEnabledParameter* TheMmmFlattenEnabledParameter;

// ----------------------------------------------------------------------------
// defectVeto: cross-panel defect veto in two-band/pyramid detail stage.
// ----------------------------------------------------------------------------

/*!
 * \brief Boolean parameter: cross-panel defect veto (params.defect_veto).
 */
class MmmDefectVetoParameter : public MetaBoolean
{
public:

   MmmDefectVetoParameter( MetaProcess* P ) : MetaBoolean( P ) {}
   IsoString Id() const override { return "defectVeto"; }
   bool DefaultValue() const override { return true; }
};

extern MmmDefectVetoParameter* TheMmmDefectVetoParameter;

// ----------------------------------------------------------------------------
// surfaceOrder: surface-fit polynomial order for the analyze stage.
// ----------------------------------------------------------------------------

/*!
 * \brief Int32 parameter: analyze surface-fit order (params.surface_order).
 */
class MmmSurfaceOrderParameter : public MetaInt32
{
public:

   MmmSurfaceOrderParameter( MetaProcess* P ) : MetaInt32( P ) {}
   IsoString Id() const override { return "surfaceOrder"; }
   double DefaultValue() const override { return 2; }
   double MinimumValue() const override { return 0; }
   double MaximumValue() const override { return 2; }
};

extern MmmSurfaceOrderParameter* TheMmmSurfaceOrderParameter;

// ----------------------------------------------------------------------------
// bandRows: output rows per delivered band (params.band_rows), advanced.
// ----------------------------------------------------------------------------

/*!
 * \brief Int32 parameter: output band granularity in rows (params.band_rows).
 */
class MmmBandRowsParameter : public MetaInt32
{
public:

   MmmBandRowsParameter( MetaProcess* P ) : MetaInt32( P ) {}
   IsoString Id() const override { return "bandRows"; }
   double DefaultValue() const override { return 256; }
   double MinimumValue() const override { return 1; }
   double MaximumValue() const override { return 65536; }
};

extern MmmBandRowsParameter* TheMmmBandRowsParameter;

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmParameters_h
