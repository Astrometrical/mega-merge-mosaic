// MmmParameters.cpp -- storage for the MmmBlendProcess MetaParameter singletons.
//
// Each pointer is set by the corresponding metaparameter's constructor (invoked
// once from InstallPixInsightModule); these definitions merely provide the
// single translation-unit storage the extern declarations in MmmParameters.h
// refer to. See that header for the parameter <-> wire mapping.

#include "MmmParameters.h"

namespace pcl
{

// ----------------------------------------------------------------------------

MmmInputImagesParameter*   TheMmmInputImagesParameter   = nullptr;
MmmViewIdParameter*        TheMmmViewIdParameter        = nullptr;
MmmFilePathsParameter*     TheMmmFilePathsParameter     = nullptr;
MmmPathParameter*          TheMmmPathParameter          = nullptr;
MmmInputSelectParameter*   TheMmmInputSelectParameter   = nullptr;
MmmSessionDirParameter*    TheMmmSessionDirParameter    = nullptr;
MmmFeatherParameter*       TheMmmFeatherParameter       = nullptr;
MmmBlendModeParameter*     TheMmmBlendModeParameter     = nullptr;
MmmFlattenParameter*       TheMmmFlattenParameter       = nullptr;
MmmFlattenEnabledParameter* TheMmmFlattenEnabledParameter = nullptr;
MmmDefectVetoParameter*    TheMmmDefectVetoParameter    = nullptr;
MmmSurfaceOrderParameter*  TheMmmSurfaceOrderParameter  = nullptr;
MmmBandRowsParameter*      TheMmmBandRowsParameter      = nullptr;

// ----------------------------------------------------------------------------

} // namespace pcl
