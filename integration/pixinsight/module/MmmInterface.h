// MmmInterface.h -- MegaMergeMosaic ProcessInterface (Task 3: full control tree).
//
// MmmBlendInterface is a static (non-dynamic), global-context, instance-
// generating ProcessInterface: it owns a private MmmBlendInstance (m_instance)
// that its controls read/write, mirroring the standard PCL idiom (cf.
// PCL_API_REFERENCE.md section 4 and the ProcessInterface::NewProcess() /
// ImportProcess() doc comments in pcl/ProcessInterface.h). The control tree is
// built lazily on first Launch() into a heap-allocated GUIData block (the
// "deferred initialization" pattern PCL recommends so unused interfaces cost
// nothing).
//
// Per spec section 10.1, the Views-vs-Files input selection is mutually
// exclusive and mode-implied (no explicit mode parameter): the interface
// tracks which side is active only as transient UI state (m_viewsMode) and
// clears the other side's instance array whenever the user switches sides or
// populates one side, matching "populating one clears/disables the other".

#ifndef __MmmInterface_h
#define __MmmInterface_h

#include <pcl/Bitmap.h>
#include <pcl/CheckBox.h>
#include <pcl/ComboBox.h>
#include <pcl/Edit.h>
#include <pcl/Label.h>
#include <pcl/NumericControl.h>
#include <pcl/ProcessInterface.h>
#include <pcl/PushButton.h>
#include <pcl/RadioButton.h>
#include <pcl/SectionBar.h>
#include <pcl/Sizer.h>
#include <pcl/SpinBox.h>
#include <pcl/ToolButton.h>
#include <pcl/TreeBox.h>

#include "MmmProcess.h"

namespace pcl
{

// ----------------------------------------------------------------------------

/*!
 * \class MmmBlendInterface
 * \brief The process interface (tool window) for MmmBlendProcess.
 *
 * Self-registers under pcl::Module on construction. Owns a private working
 * MmmBlendInstance whose parameters the child controls edit directly (both
 * classes are declared friends of each other via MmmBlendInstance's
 * `friend class MmmBlendInterface;`), then hands out copies of it through
 * NewProcess() and adopts foreign instances via ImportProcess().
 */
class MmmBlendInterface : public ProcessInterface
{
public:

   MmmBlendInterface();
   ~MmmBlendInterface() override;

   IsoString              Id() const override;
   MetaProcess*           Process() const override;
   InterfaceFeatures      Features() const override;
   ProcessImplementation* NewProcess() const override;
   bool                   ImportProcess( const ProcessImplementation& ) override;
   bool                   Launch( const MetaProcess&, const ProcessImplementation*,
                                  bool& dynamic, unsigned& flags ) override;
   IsoString              IconImageSVG() const override;

private:

   /*!
    * \struct GUIData
    * \brief All child controls of the interface, built once on first Launch().
    */
   struct GUIData
   {
      GUIData( MmmBlendInterface& );

      VerticalSizer   Global_Sizer;

      // --- Header notice: logo + title/tagline/copyright ---------------------
      Control         Notice_Control;
      HorizontalSizer Notice_Sizer;
      Control         Logo_Control;          // paints the chevron bitmap
      VerticalSizer   NoticeText_Sizer;
      Label           Title_Label;
      Label           Tagline_Label;
      Label           Copyright_Label;
      Bitmap          Logo_Bitmap;           // rendered from MMM_CHEVRON_SVG

      // --- Target Frames section --------------------------------------------
      SectionBar      TargetFrames_SectionBar;
      Control         TargetFrames_Control;
      VerticalSizer   TargetFrames_Sizer;
      HorizontalSizer InputMode_Sizer;
      RadioButton     ViewsMode_RadioButton;
      RadioButton     FilesMode_RadioButton;
      TreeBox         Views_TreeBox;
      HorizontalSizer ViewButtons_Sizer;
      PushButton      AddViews_PushButton;
      PushButton      RemoveView_PushButton;
      TreeBox         Files_TreeBox;
      HorizontalSizer FileButtons_Sizer;
      PushButton      AddFiles_PushButton;
      PushButton      RemoveFile_PushButton;

      // --- Parameters section ------------------------------------------------
      SectionBar      Parameters_SectionBar;
      Control         Parameters_Control;
      VerticalSizer   Parameters_Sizer;
      HorizontalSizer SessionDir_Sizer;
      Label           SessionDir_Label;
      Edit            SessionDir_Edit;
      ToolButton      SessionDir_ToolButton;
      HorizontalSizer InputSelect_Sizer;
      Label           InputSelect_Label;
      ComboBox        InputSelect_ComboBox;
      HorizontalSizer BlendMode_Sizer;
      Label           BlendMode_Label;
      ComboBox        BlendMode_ComboBox;
      NumericControl  Feather_NumericControl;
      HorizontalSizer SurfaceOrder_Sizer;
      Label           SurfaceOrder_Label;
      SpinBox         SurfaceOrder_SpinBox;
      HorizontalSizer BandRows_Sizer;
      Label           BandRows_Label;
      SpinBox         BandRows_SpinBox;
      HorizontalSizer DefectVeto_Sizer;
      CheckBox        DefectVeto_CheckBox;
      HorizontalSizer Flatten_Sizer;
      CheckBox        FlattenEnabled_CheckBox;
      SpinBox         FlattenOrder_SpinBox;
   };

   GUIData* GUI = nullptr;

   // The interface's own working instance: NewProcess() duplicates it;
   // ImportProcess()/Launch() populate it and then refresh the controls from
   // it via UpdateControls().
   MmmBlendInstance m_instance;

   // Transient UI-only state: which side of the Views/Files toggle is active.
   // Not part of MmmBlendInstance -- the mutual exclusion is expressed by
   // always keeping the *other* side's array empty, per spec 10.1.
   bool m_viewsMode = true;

   void UpdateControls();
   void UpdateInputModeControls();
   void UpdateFlattenControls();
   void PopulateViewsTreeBox();
   void PopulateFilesTreeBox();

   // --- Event handlers (direct OnXxx idiom, no __CLASS_HANDLER macro) --------

   void e_ModeClick( Button& sender, bool checked );
   void e_AddViewsClick( Button& sender, bool checked );
   void e_RemoveViewClick( Button& sender, bool checked );
   void e_AddFilesClick( Button& sender, bool checked );
   void e_RemoveFileClick( Button& sender, bool checked );

   void e_SessionDirEditCompleted( Edit& sender );
   void e_SessionDirBrowseClick( Button& sender, bool checked );

   void e_InputSelectItemSelected( ComboBox& sender, int itemIndex );
   void e_BlendModeItemSelected( ComboBox& sender, int itemIndex );

   void e_FeatherValueUpdated( NumericEdit& sender, double value );
   void e_SurfaceOrderValueUpdated( SpinBox& sender, int value );
   void e_BandRowsValueUpdated( SpinBox& sender, int value );

   void e_DefectVetoClick( Button& sender, bool checked );
   void e_FlattenEnabledClick( Button& sender, bool checked );
   void e_FlattenOrderValueUpdated( SpinBox& sender, int value );

   void e_ToggleSection( SectionBar& sender, Control& section, bool start );
   void e_NoticeMouseRelease( Control& sender, const pcl::Point& pos,
                              int button, unsigned buttons, unsigned modifiers );
   void e_LogoPaint( Control& sender, const pcl::Rect& updateRect );
};

/*!
 * \brief The MegaMergeMosaic interface singleton.
 *
 * Instantiated once by InstallPixInsightModule(); non-owning global handle.
 */
extern MmmBlendInterface* TheMmmBlendInterface;

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmInterface_h
