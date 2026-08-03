// mmm.cpp -- MegaMergeMosaic PixInsight module (MmmModule) + installation entry point.
//
// Per PCL_API_REFERENCE.md section 1: the three module entry points have C
// linkage and are free functions, not members of MetaModule. PixInsight's own
// generated glue supplies IdentifyPixInsightModule (PMIDN) and
// InitializePixInsightModule (PMINI); a hand-written module only defines
// InstallPixInsightModule (PMINS).
//
// Constructing MmmModule sets the pcl::Module global; constructing the process
// and interface singletons afterwards self-registers them under that module.

#include <pcl/MetaModule.h>

#include "MmmInterface.h"
#include "MmmParameters.h"
#include "MmmProcess.h"
#include "MmmVersion.h"

#define MMM_MODULE_VERSION_LANGUAGE  eng

namespace pcl
{

// ----------------------------------------------------------------------------

/*!
 * \class MmmModule
 * \brief The MegaMergeMosaic module meta-object (tree root).
 */
class MmmModule : public MetaModule
{
public:

   MmmModule() = default;

   const char* Version() const override
   {
      return PCL_MODULE_VERSION( MMM_VERSION_MAJOR,
                                 MMM_VERSION_MINOR,
                                 MMM_VERSION_REVISION,
                                 MMM_VERSION_BUILD,
                                 MMM_MODULE_VERSION_LANGUAGE );
   }

   IsoString Name() const override
   {
      return "MegaMergeMosaic";
   }

   String Description() const override
   {
      return "Mega Merge Mosaic: fast merge/blend for pre-aligned astro mosaic panels.";
   }

   String Company() const override
   {
      return "Astrometrical";
   }

   String Author() const override
   {
      return "Daniel Paull";
   }

   String Copyright() const override
   {
      return "Copyright (c) 2026 Astrometrical";
   }
};

// ----------------------------------------------------------------------------

} // namespace pcl

// ----------------------------------------------------------------------------

/*
 * Module installation routine (PMINS). Called by the PixInsight core to
 * create and register the module's meta-objects.
 */
PCL_MODULE_EXPORT pcl::int32 InstallPixInsightModule( pcl::int32 mode )
{
   new pcl::MmmModule;   // sets pcl::Module

   if ( mode == pcl::InstallMode::FullInstall )
   {
      new pcl::MmmBlendProcess;     // self-registers under pcl::Module

      // Parameter singletons self-register under their owning MetaProcess* (or,
      // for a table string column, its MetaTable*) via MetaObject's ctor. Each
      // TheMmm...Parameter global captures the singleton for pointer-identity
      // dispatch in the instance hooks. Construct each table before its string
      // column so the column is appended to the correct parent.
      using namespace pcl;
      TheMmmInputImagesParameter   = new MmmInputImagesParameter( TheMmmBlendProcess );
      TheMmmViewIdParameter        = new MmmViewIdParameter( TheMmmInputImagesParameter );
      TheMmmFilePathsParameter     = new MmmFilePathsParameter( TheMmmBlendProcess );
      TheMmmPathParameter          = new MmmPathParameter( TheMmmFilePathsParameter );
      TheMmmInputSelectParameter   = new MmmInputSelectParameter( TheMmmBlendProcess );
      TheMmmSessionDirParameter    = new MmmSessionDirParameter( TheMmmBlendProcess );
      TheMmmFeatherParameter       = new MmmFeatherParameter( TheMmmBlendProcess );
      TheMmmBlendModeParameter     = new MmmBlendModeParameter( TheMmmBlendProcess );
      TheMmmFlattenParameter       = new MmmFlattenParameter( TheMmmBlendProcess );
      TheMmmFlattenEnabledParameter = new MmmFlattenEnabledParameter( TheMmmBlendProcess );
      TheMmmDefectVetoParameter    = new MmmDefectVetoParameter( TheMmmBlendProcess );
      TheMmmSurfaceOrderParameter  = new MmmSurfaceOrderParameter( TheMmmBlendProcess );
      TheMmmBandRowsParameter      = new MmmBandRowsParameter( TheMmmBlendProcess );

      new MmmBlendInterface;   // self-registers under pcl::Module
   }

   return 0;
}
