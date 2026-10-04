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
   void                   ResetInstance() override;
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
      HorizontalSizer Links_Sizer;
      Label           ShareLink_Label;       // -> https://astrometrical.com
      Label           LinkSep1_Label;
      Label           ToolsLink_Label;       // -> https://tools.astrometrical.com/
      Label           LinkSep2_Label;
      Label           KofiLink_Label;        // -> https://ko-fi.com/astrometrical
      Bitmap          Logo_Bitmap;           // rendered from MMM_CHEVRON_SVG

      // --- Target Frames section --------------------------------------------
      // Declaration order matters for every Control/SectionBar pair below:
      // ~SectionBar calls through its section (Control::OnShow/OnHide on the
      // section's handle), and members are destroyed in reverse declaration
      // order, so each section Control MUST be declared BEFORE the SectionBar
      // that SetSection()s it. Enforced by host/test/test_sectionbar_order.
      Control         TargetFrames_Control;
      SectionBar      TargetFrames_SectionBar;
      VerticalSizer   TargetFrames_Sizer;
      HorizontalSizer InputMode_Sizer;
      RadioButton     ViewsMode_RadioButton;
      RadioButton     FilesMode_RadioButton;
      Label           Filter_Label;          // wildcard filter on the list below
      Edit            Filter_Edit;
      TreeBox         Views_TreeBox;
      HorizontalSizer ViewButtons_Sizer;
      PushButton      AddViews_PushButton;
      PushButton      RemoveView_PushButton;
      TreeBox         Files_TreeBox;
      HorizontalSizer FileButtons_Sizer;
      PushButton      AddFiles_PushButton;
      PushButton      RemoveFile_PushButton;

      // --- Multi-Mosaic - Panel Grouping section -----------------------------
      // Three actions, one per row, each with a one-line explanation: the
      // feature is new to mosaic tools, so the text carries the meaning.
      // Control before SectionBar (see the Target Frames note above).
      Control         PanelGroups_Control;
      SectionBar      PanelGroups_SectionBar;
      VerticalSizer   PanelGroups_Sizer;
      Label           PanelGroupsIntro_Label;
      HorizontalSizer GroupByFilter_Sizer;
      PushButton      GroupByFilter_PushButton;
      Label           GroupByFilter_Label;
      HorizontalSizer SetGroup_Sizer;
      Edit            Group_Edit;
      PushButton      SetGroup_PushButton;
      Label           SetGroup_Label;
      HorizontalSizer ClearGroups_Sizer;
      PushButton      ClearGroups_PushButton;
      Label           ClearGroups_Label;

      // --- Parameters section ------------------------------------------------
      Control         Parameters_Control;
      SectionBar      Parameters_SectionBar;
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
      HorizontalSizer GainMode_Sizer;
      Label           GainMode_Label;
      ComboBox        GainMode_ComboBox;
      HorizontalSizer BandRows_Sizer;
      Label           BandRows_Label;
      SpinBox         BandRows_SpinBox;
      HorizontalSizer DefectVeto_Sizer;
      CheckBox        DefectVeto_CheckBox;
      HorizontalSizer SeamMap_Sizer;
      CheckBox        SeamMap_CheckBox;
      HorizontalSizer Flatten_Sizer;
      CheckBox        FlattenEnabled_CheckBox;
      SpinBox         FlattenOrder_SpinBox;

      // --- Advanced section ---------------------------------------------------
      Control         Advanced_Control;
      SectionBar      Advanced_SectionBar;
      VerticalSizer   Advanced_Sizer;
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

   // Target Frames filter: the live wildcard pattern (empty = show all), the
   // emulated placeholder state of Filter_Edit (PCL Edit has no native
   // placeholder), and the instance rows currently displayed, in TreeBox
   // node order (so node index i maps to instance row m_visibleRows[i]).
   String     m_filterPattern;
   bool       m_filterHintShown = false;
   Array<int> m_visibleRows;
   // Group by FILTER (Files mode) runs a pumped worker probe: this flag
   // rejects re-entrant clicks delivered by that pump, and the whole
   // interface is disabled for the probe's duration.
   bool       m_probeInProgress = false;

   static constexpr const char* kFilterHint = "e.g. *_Ha*";

   void UpdateControls();
   void UpdateInputModeControls();
   void UpdateFlattenControls();
   void PopulateActiveTreeBox();
   Array<int>     TargetRows() const;   // selection if any, else all displayed rows
   Array<String>& ActiveGroups();       // p_viewGroups or p_fileGroups
   Array<String>& ActiveItems();        // p_viewIds or p_filePaths
   void           ShowFilterHint( bool show );

   // --- Event handlers (direct OnXxx idiom, no __CLASS_HANDLER macro) --------

   void e_ModeClick( Button& sender, bool checked );
   void e_AddViewsClick( Button& sender, bool checked );
   void e_RemoveViewClick( Button& sender, bool checked );
   void e_AddFilesClick( Button& sender, bool checked );
   void e_RemoveFileClick( Button& sender, bool checked );

   void e_FilterTextUpdated( Edit& sender, const String& text );
   void e_FilterGetFocus( Control& sender );
   void e_FilterLoseFocus( Control& sender );
   void e_SetGroupClick( Button& sender, bool checked );
   void e_GroupByFilterClick( Button& sender, bool checked );
   void e_ClearGroupsClick( Button& sender, bool checked );

   void e_SessionDirEditCompleted( Edit& sender );
   void e_SessionDirBrowseClick( Button& sender, bool checked );

   void e_InputSelectItemSelected( ComboBox& sender, int itemIndex );
   void e_BlendModeItemSelected( ComboBox& sender, int itemIndex );

   void e_FeatherValueUpdated( NumericEdit& sender, double value );
   void e_SurfaceOrderValueUpdated( SpinBox& sender, int value );
   void e_GainModeItemSelected( ComboBox& sender, int itemIndex );
   void e_BandRowsValueUpdated( SpinBox& sender, int value );

   void e_DefectVetoClick( Button& sender, bool checked );
   void e_SeamMapClick( Button& sender, bool checked );
   void e_FlattenEnabledClick( Button& sender, bool checked );
   void e_FlattenOrderValueUpdated( SpinBox& sender, int value );

   void e_ToggleSection( SectionBar& sender, Control& section, bool start );
   void e_LinkMouseRelease( Control& sender, const pcl::Point& pos,
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
