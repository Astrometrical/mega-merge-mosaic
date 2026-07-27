// mmm.cpp -- MergeMosaic PixInsight module (MmmModule) + installation entry point.
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
#include "MmmProcess.h"

#define MMM_MODULE_VERSION_MAJOR     1
#define MMM_MODULE_VERSION_MINOR     0
#define MMM_MODULE_VERSION_REVISION  0
#define MMM_MODULE_VERSION_BUILD     1
#define MMM_MODULE_VERSION_LANGUAGE  eng

namespace pcl
{

// ----------------------------------------------------------------------------

/*!
 * \class MmmModule
 * \brief The MergeMosaic module meta-object (tree root).
 */
class MmmModule : public MetaModule
{
public:

   MmmModule() = default;

   const char* Version() const override
   {
      return PCL_MODULE_VERSION( MMM_MODULE_VERSION_MAJOR,
                                 MMM_MODULE_VERSION_MINOR,
                                 MMM_MODULE_VERSION_REVISION,
                                 MMM_MODULE_VERSION_BUILD,
                                 MMM_MODULE_VERSION_LANGUAGE );
   }

   IsoString Name() const override
   {
      return "MergeMosaic";
   }

   String Description() const override
   {
      return "MergeMosaic: fast merge/blend for pre-aligned astro mosaic panels.";
   }

   String Company() const override
   {
      return "MergeMosaic";
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
      new pcl::MmmBlendInterface;   // self-registers under pcl::Module
   }

   return 0;
}
