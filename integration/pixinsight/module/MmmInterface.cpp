// MmmInterface.cpp -- MegaMergeMosaic ProcessInterface implementation (Task 3).
//
// Builds the real control tree (deferred-init GUIData, per PCL_API_REFERENCE.md
// section 4) and wires every control to the interface's private working
// MmmBlendInstance (m_instance) using the direct OnXxx(handler, receiver)
// idiom -- there is no __CLASS_HANDLER macro in PCL. NewProcess() hands out
// copies of m_instance; ImportProcess() adopts a foreign instance's values
// into m_instance and refreshes the controls.
//
// Mutual exclusion of the Views/Files input (spec section 10.1): the
// interface never lets both p_viewIds and p_filePaths be non-empty at once.
// Switching the toggle, or adding to one side, clears the other side's array
// on the working instance; UpdateControls() re-syncs both radio buttons and
// both TreeBoxes' enabled state unconditionally, so the displayed state can
// never drift from the interface's own model even if the platform's native
// RadioButton exclusivity behaves differently than expected (unconfirmed
// against the reference doc -- see task-3-report.md).

#include "MmmInterface.h"
#include "MmmExecution.h"
#include "MmmGroups.h"
#include "MmmIcon.h"
#include "MmmVersion.h"

#include <string>
#include <vector>

#include <pcl/Array.h>
#include <pcl/Console.h>
#include <pcl/Cursor.h>
#include <pcl/ExternalProcess.h>
#include <pcl/FITSHeaderKeyword.h>
#include <pcl/File.h>
#include <pcl/FileDialog.h>
#include <pcl/Graphics.h>
#include <pcl/MultiViewSelectionDialog.h>
#include <pcl/StringList.h>
#include <pcl/View.h>

namespace pcl
{

// ----------------------------------------------------------------------------

MmmBlendInterface* TheMmmBlendInterface = nullptr;

// ----------------------------------------------------------------------------

MmmBlendInterface::MmmBlendInterface()
   : m_instance( TheMmmBlendProcess )
{
   // Constructing a ProcessInterface self-registers it under pcl::Module.
   TheMmmBlendInterface = this;
}

// NOTE: this destructor is unreachable in practice. The interface is `new`ed
// in InstallPixInsightModule and owned by pcl::Module's MetaObject child
// list; nothing in PCL (GlobalContextDispatcher::OnUnload only saves
// geometry/settings) or in this module ever deletes pcl::Module, and the
// core tears the process down with the heap intact. ~GUIData is nonetheless
// kept safe to run: every section Control is declared before its SectionBar
// (see the note in MmmInterface.h) so ~SectionBar never calls through a
// destroyed control -- the pattern that aborted PixInsight in mpp, where the
// dialog was a stack object.
MmmBlendInterface::~MmmBlendInterface()
{
   delete GUI;
}

IsoString MmmBlendInterface::Id() const
{
   return "MegaMergeMosaic";
}

MetaProcess* MmmBlendInterface::Process() const
{
   return TheMmmBlendProcess;
}

InterfaceFeatures MmmBlendInterface::Features() const
{
   // Static interface that executes in the global context.
   return InterfaceFeature::DefaultGlobal;
}

ProcessImplementation* MmmBlendInterface::NewProcess() const
{
   return new MmmBlendInstance( m_instance );
}

bool MmmBlendInterface::ImportProcess( const ProcessImplementation& p )
{
   const MmmBlendInstance* instance = dynamic_cast<const MmmBlendInstance*>( &p );
   if ( instance == nullptr )
      return false;

   m_instance.Assign( *instance );

   // Re-derive the transient Views-vs-Files toggle from which side actually
   // carries data (there is no explicit mode member on the instance -- see
   // spec 10.1). Default to Views mode if both sides happen to be empty.
   m_viewsMode = !m_instance.p_viewIds.IsEmpty() || m_instance.p_filePaths.IsEmpty();

   // Nothing in Assign()/the copy constructor prevents a foreign (legacy or
   // scripted) instance from carrying BOTH arrays populated at once. Enforce
   // the same single-input-type invariant here that every other mutation
   // point (e_ModeClick, e_AddViewsClick, e_AddFilesClick) enforces: clear
   // whichever side m_viewsMode did NOT select.
   if ( m_viewsMode )
   {
      m_instance.p_filePaths.Clear();
      m_instance.p_fileGroups.Clear();
   }
   else
   {
      m_instance.p_viewIds.Clear();
      m_instance.p_viewGroups.Clear();
   }

   if ( GUI != nullptr )
      UpdateControls();

   return true;
}

void MmmBlendInterface::ResetInstance()
{
   // The control bar's Reset button. Import a default-constructed instance;
   // ImportProcess() re-derives the Views/Files mode and refreshes controls.
   MmmBlendInstance defaultInstance( TheMmmBlendProcess );
   ImportProcess( defaultInstance );
}

bool MmmBlendInterface::Launch( const MetaProcess&, const ProcessImplementation*,
                                 bool& dynamic, unsigned& /*flags*/ )
{
   // Deferred initialization: build the control tree only the first time the
   // interface is actually shown. Per the ProcessInterface::Launch() doc, the
   // core calls ValidateProcess()/ImportProcess() itself right after a
   // successful Launch() from an existing instance, so this function does not
   // need to import `instance` explicitly.
   if ( GUI == nullptr )
   {
      GUI = new GUIData( *this );
      SetWindowTitle( "Mega Merge Mosaic" );
      UpdateControls();
      ShowFilterHint( true );
   }

   dynamic = false;
   return true;
}

IsoString MmmBlendInterface::IconImageSVG() const
{
   return MMM_PROCESS_ICON_SVG;
}

// ----------------------------------------------------------------------------
// GUIData -- control tree construction + event wiring.
// ----------------------------------------------------------------------------

MmmBlendInterface::GUIData::GUIData( MmmBlendInterface& w )
{
   int labelWidth1 = w.Font().Width( String( "Panel registration method:" ) + 'M' );
   int editWidth1  = w.Font().Width( String( '0', 8 ) );

   //
   // Header notice: chevron logo + title / tagline / copyright, in the style
   // of the MosaicByCoordinates script header.
   //
   Logo_Bitmap = Bitmap( MMM_CHEVRON_SVG, sizeof( MMM_CHEVRON_SVG ) - 1, "SVG" );

   Logo_Control.SetScaledFixedSize( 40, 40 );
   Logo_Control.OnPaint( (Control::paint_event_handler)&MmmBlendInterface::e_LogoPaint, w );

   Title_Label.SetText( "Mega Merge Mosaic version " MMM_VERSION_STRING );
   Title_Label.SetStyleSheet( w.ScaledStyleSheet( "QLabel { font-weight: bold; }" ) );

   Tagline_Label.SetText( "Big mosaics, no big deal." );

   Copyright_Label.SetText( "Copyright (c) 2026 Astrometrical" );

   // Hyperlink-styled labels: colored + underlined so they read as links,
   // pointing-hand cursor, and a shared mouse-release handler that dispatches
   // on the sender. The color works on both the dark (default) and light core
   // themes.
   auto makeLink = [&w]( Label& label, const String& text, const String& toolTip )
   {
      label.SetText( text );
      label.SetToolTip( toolTip );
      label.SetCursor( StdCursor::PointingHand );
      label.SetStyleSheet( "QLabel { color: #4FA6FF; text-decoration: underline; }" );
      label.OnMouseRelease( (Control::mouse_button_event_handler)&MmmBlendInterface::e_LinkMouseRelease, w );
   };
   makeLink( ShareLink_Label, "Share your mosaics on Astrometrical",
             "<p>Visit https://astrometrical.com</p>" );
   makeLink( ToolsLink_Label, "Astrometrical Tools",
             "<p>Visit https://tools.astrometrical.com/</p>" );
   makeLink( KofiLink_Label, "Buy me a coffee",
             "<p>Support development: https://ko-fi.com/astrometrical</p>" );

   LinkSep1_Label.SetText( u"\x00B7" ); // middle dot separator
   LinkSep2_Label.SetText( u"\x00B7" );

   Links_Sizer.SetSpacing( 6 );
   Links_Sizer.Add( ShareLink_Label );
   Links_Sizer.Add( LinkSep1_Label );
   Links_Sizer.Add( ToolsLink_Label );
   Links_Sizer.Add( LinkSep2_Label );
   Links_Sizer.Add( KofiLink_Label );
   Links_Sizer.AddStretch();

   NoticeText_Sizer.SetSpacing( 2 );
   NoticeText_Sizer.Add( Title_Label );
   NoticeText_Sizer.Add( Tagline_Label );
   NoticeText_Sizer.Add( Copyright_Label );
   NoticeText_Sizer.Add( Links_Sizer );

   Notice_Sizer.SetMargin( 6 );
   Notice_Sizer.SetSpacing( 8 );
   Notice_Sizer.Add( Logo_Control );
   Notice_Sizer.Add( NoticeText_Sizer, 100 );
   Notice_Control.SetSizer( Notice_Sizer );

   //
   // Target Frames section.
   //
   ViewsMode_RadioButton.SetText( "Views" );
   ViewsMode_RadioButton.SetChecked();
   ViewsMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );
   ViewsMode_RadioButton.SetToolTip( "<p>Blend image views that are open in the current PixInsight workspace.</p>" );

   FilesMode_RadioButton.SetText( "Files" );
   FilesMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );
   FilesMode_RadioButton.SetToolTip( "<p>Blend image files read directly from disk, without opening them as views.</p>" );

   InputMode_Sizer.SetSpacing( 8 );
   InputMode_Sizer.Add( ViewsMode_RadioButton );
   InputMode_Sizer.Add( FilesMode_RadioButton );
   InputMode_Sizer.AddStretch();

   Views_TreeBox.SetNumberOfColumns( 2 );
   Views_TreeBox.SetHeaderText( 0, "View Id" );
   Views_TreeBox.SetHeaderText( 1, "Group" );
   Views_TreeBox.EnableMultipleSelections();
   Views_TreeBox.SetScaledMinSize( 400, 120 );
   Views_TreeBox.SetToolTip( "<p>The mosaic panels to merge. All panels must belong to the same mosaic: "
      "registered full-canvas panels (e.g. MosaicByCoordinates output) or plate-solved panels.</p>" );

   AddViews_PushButton.SetText( "Add Views..." );
   AddViews_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddViewsClick, w );
   AddViews_PushButton.SetToolTip( "<p>Add open views as mosaic panels.</p>" );

   RemoveView_PushButton.SetText( "Remove" );
   RemoveView_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveViewClick, w );
   RemoveView_PushButton.SetToolTip( "<p>Remove the selected views from the list.</p>" );

   ViewButtons_Sizer.SetSpacing( 6 );
   ViewButtons_Sizer.Add( AddViews_PushButton );
   ViewButtons_Sizer.Add( RemoveView_PushButton );
   ViewButtons_Sizer.AddStretch();

   Files_TreeBox.SetNumberOfColumns( 2 );
   Files_TreeBox.SetHeaderText( 0, "File Path" );
   Files_TreeBox.SetHeaderText( 1, "Group" );
   Files_TreeBox.EnableMultipleSelections();
   Files_TreeBox.SetScaledMinSize( 400, 120 );
   Files_TreeBox.SetToolTip( "<p>The mosaic panel files to merge. All panels must belong to the same mosaic: "
      "registered full-canvas panels (e.g. MosaicByCoordinates output) or plate-solved panels.</p>" );

   AddFiles_PushButton.SetText( "Add Files..." );
   AddFiles_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddFilesClick, w );
   AddFiles_PushButton.SetToolTip( "<p>Add image files (XISF or FITS) as mosaic panels.</p>" );

   RemoveFile_PushButton.SetText( "Remove" );
   RemoveFile_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveFileClick, w );
   RemoveFile_PushButton.SetToolTip( "<p>Remove the selected files from the list.</p>" );

   FileButtons_Sizer.SetSpacing( 6 );
   FileButtons_Sizer.Add( AddFiles_PushButton );
   FileButtons_Sizer.Add( RemoveFile_PushButton );
   FileButtons_Sizer.AddStretch();

   Filter_Label.SetText( "Filter:" );
   Filter_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );
   Filter_Edit.SetToolTip( "<p>Show only the panels whose view id or file name matches this "
      "wildcard pattern (* = any run, ? = one character, case-insensitive), e.g. <b>*_Ha*</b>. "
      "Then press <b>Set group</b> to assign every displayed panel to a group at once.</p>" );
   Filter_Edit.OnTextUpdated( (Edit::text_event_handler)&MmmBlendInterface::e_FilterTextUpdated, w );
   Filter_Edit.OnGetFocus( (Control::event_handler)&MmmBlendInterface::e_FilterGetFocus, w );
   Filter_Edit.OnLoseFocus( (Control::event_handler)&MmmBlendInterface::e_FilterLoseFocus, w );

   Group_Label.SetText( "Group:" );
   Group_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );
   Group_Edit.SetToolTip( "<p>Group name to assign with <b>Set group</b>. Each group is merged into "
      "its own output window (MegaMergeMosaic_&lt;group&gt;); all groups share one reference frame "
      "so the outputs can be combined directly. Leave empty for the default group.</p>" );

   SetGroup_PushButton.SetText( "Set group" );
   SetGroup_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_SetGroupClick, w );
   SetGroup_PushButton.SetToolTip( "<p>Assign the group name to the selected panels, or to every "
      "displayed panel when nothing is selected.</p>" );

   GroupByFilter_PushButton.SetText( "Group by FILTER" );
   GroupByFilter_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_GroupByFilterClick, w );
   GroupByFilter_PushButton.SetToolTip( "<p>Fill each panel's group from its FILTER keyword "
      "(selected panels, or every displayed panel when nothing is selected). Panels without a "
      "FILTER keyword keep their current group.</p>" );

   ClearGroups_PushButton.SetText( "Clear groups" );
   ClearGroups_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ClearGroupsClick, w );
   ClearGroups_PushButton.SetToolTip( "<p>Move the selected panels (or every displayed panel when "
      "nothing is selected) back to the default group.</p>" );

   GroupTools_Sizer.SetSpacing( 6 );
   GroupTools_Sizer.Add( Filter_Label );
   GroupTools_Sizer.Add( Filter_Edit, 100 );
   GroupTools_Sizer.Add( Group_Label );
   GroupTools_Sizer.Add( Group_Edit, 100 );
   GroupTools_Sizer.Add( SetGroup_PushButton );
   GroupTools_Sizer.Add( GroupByFilter_PushButton );
   GroupTools_Sizer.Add( ClearGroups_PushButton );

   TargetFrames_Sizer.SetSpacing( 4 );
   TargetFrames_Sizer.Add( InputMode_Sizer );
   TargetFrames_Sizer.Add( Views_TreeBox );
   TargetFrames_Sizer.Add( ViewButtons_Sizer );
   TargetFrames_Sizer.Add( Files_TreeBox );
   TargetFrames_Sizer.Add( FileButtons_Sizer );
   TargetFrames_Sizer.Add( GroupTools_Sizer );
   TargetFrames_Control.SetSizer( TargetFrames_Sizer );

   TargetFrames_SectionBar.SetTitle( "Target Frames" );
   TargetFrames_SectionBar.SetSection( TargetFrames_Control );
   TargetFrames_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );

   //
   // Parameters section.
   //
   SessionDir_Label.SetText( "Session directory:" );
   SessionDir_Label.SetFixedWidth( labelWidth1 );
   SessionDir_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   SessionDir_Edit.OnEditCompleted( (Edit::edit_event_handler)&MmmBlendInterface::e_SessionDirEditCompleted, w );
   SessionDir_Edit.SetToolTip( "<p>Optional working directory for the analysis cache "
      "(a *.mmm-session folder).</p>"
      "<p>Leave empty (default) to use a temporary directory that is removed automatically "
      "when the run finishes. Set a directory to keep the cache: re-running with the same "
      "directory and inputs resumes completed analysis stages.</p>" );

   SessionDir_ToolButton.SetIcon( Bitmap( w.ScaledResource( ":/browser/select-file.png" ) ) );
   SessionDir_ToolButton.SetScaledFixedSize( 20, 20 );
   SessionDir_ToolButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_SessionDirBrowseClick, w );
   SessionDir_ToolButton.SetToolTip( "<p>Select the optional session directory.</p>" );

   SessionDir_Sizer.SetSpacing( 4 );
   SessionDir_Sizer.Add( SessionDir_Label );
   SessionDir_Sizer.Add( SessionDir_Edit, 100 );
   SessionDir_Sizer.Add( SessionDir_ToolButton );

   //
   // Input override (advanced): Auto / Aligned / Solved.
   // Element order MUST match MmmInputSelectParameter (Auto=0/Aligned=1/Solved=2).
   //
   InputSelect_Label.SetText( "Panel registration method:" );
   InputSelect_Label.SetFixedWidth( labelWidth1 );
   InputSelect_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   InputSelect_ComboBox.AddItem( "Auto" );
   InputSelect_ComboBox.AddItem( "Pre-aligned (MosaicByCoordinates)" );
   InputSelect_ComboBox.AddItem( "Align by astrometric solution" );
   InputSelect_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_InputSelectItemSelected, w );
   InputSelect_ComboBox.SetMinWidth( editWidth1*2 );
   InputSelect_ComboBox.SetToolTip( "<p>How panels are placed on the output canvas:</p>"
      "<p><b>Auto</b> - detect the correct mode automatically from the inputs.</p>"
      "<p><b>Pre-aligned (MosaicByCoordinates)</b> - panels are registered full-canvas frames "
      "sharing the same dimensions (e.g. MosaicByCoordinates output); blend them directly.</p>"
      "<p><b>Align by astrometric solution</b> - reproject each panel onto a common frame "
      "using its astrometric solution (every panel must be plate-solved).</p>" );

   InputSelect_Sizer.SetSpacing( 4 );
   InputSelect_Sizer.Add( InputSelect_Label );
   InputSelect_Sizer.Add( InputSelect_ComboBox );
   InputSelect_Sizer.AddStretch();

   //
   // Blend parameters.
   //
   BlendMode_Label.SetText( "Blend mode:" );
   BlendMode_Label.SetFixedWidth( labelWidth1 );
   BlendMode_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   // Element order MUST match MmmBlendModeParameter (Feather=0/TwoBand=1/Pyramid=2).
   BlendMode_ComboBox.AddItem( "Feather" );
   BlendMode_ComboBox.AddItem( "TwoBand" );
   BlendMode_ComboBox.AddItem( "Pyramid" );
   BlendMode_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_BlendModeItemSelected, w );
   BlendMode_ComboBox.SetMinWidth( editWidth1*2 );
   BlendMode_ComboBox.SetToolTip( "<p><b>Feather</b> - weighted-average ramp across the overlap.</p>"
      "<p><b>TwoBand</b> - low frequencies feathered, high frequencies seam-cut.</p>"
      "<p><b>Pyramid</b> - full multiband blend; best quality (default).</p>" );

   BlendMode_Sizer.SetSpacing( 4 );
   BlendMode_Sizer.Add( BlendMode_Label );
   BlendMode_Sizer.Add( BlendMode_ComboBox );
   BlendMode_Sizer.AddStretch();

   Feather_NumericControl.label.SetText( "Feather:" );
   Feather_NumericControl.label.SetFixedWidth( labelWidth1 );
   Feather_NumericControl.edit.SetFixedWidth( editWidth1 );
   Feather_NumericControl.SetInteger();
   Feather_NumericControl.SetRange( 1, 1024 );
   Feather_NumericControl.OnValueUpdated( (NumericEdit::value_event_handler)&MmmBlendInterface::e_FeatherValueUpdated, w );
   Feather_NumericControl.SetToolTip( "<p>Feather ramp length in canvas pixels (1-1024). "
      "Larger values give smoother low-frequency transitions across panel overlaps.</p>" );

   SurfaceOrder_Label.SetText( "Gradient fit order:" );
   SurfaceOrder_Label.SetFixedWidth( labelWidth1 );
   SurfaceOrder_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );
   SurfaceOrder_SpinBox.SetRange( 0, 2 );
   SurfaceOrder_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_SurfaceOrderValueUpdated, w );
   SurfaceOrder_SpinBox.SetFixedWidth( editWidth1 );
   SurfaceOrder_SpinBox.SetToolTip( "<p>Polynomial order (0-2) of the per-panel gradient fit used "
      "to match panel backgrounds before blending: 0 = constant offset, 1 = plane, "
      "2 = quadratic (default).</p>" );

   SurfaceOrder_Sizer.SetSpacing( 4 );
   SurfaceOrder_Sizer.Add( SurfaceOrder_Label );
   SurfaceOrder_Sizer.Add( SurfaceOrder_SpinBox );
   SurfaceOrder_Sizer.AddStretch();

   GainMode_Label.SetText( "Gain mode:" );
   GainMode_Label.SetFixedWidth( labelWidth1 );
   GainMode_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   // Element order MUST match MmmGainModeParameter (Fit=0/Unity=1).
   GainMode_ComboBox.AddItem( "Fit" );
   GainMode_ComboBox.AddItem( "Unity" );
   GainMode_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_GainModeItemSelected, w );
   GainMode_ComboBox.SetMinWidth( editWidth1*2 );
   GainMode_ComboBox.SetToolTip( "<p>How the photometric solve treats per-panel brightness gains.</p>"
      "<p><b>Fit</b> (default) - measure a gain factor for each panel from the overlap regions, "
      "correcting real transparency and exposure differences between panels.</p>"
      "<p><b>Unity</b> - force every gain to 1 and match panels with offsets only. Choose this for "
      "mosaics known to be photometrically homogeneous (same rig, exposure and filter, stable "
      "skies), where a fitted gain could only chase noise.</p>" );

   GainMode_Sizer.SetSpacing( 4 );
   GainMode_Sizer.Add( GainMode_Label );
   GainMode_Sizer.Add( GainMode_ComboBox );
   GainMode_Sizer.AddStretch();

   BandRows_Label.SetText( "Band rows:" );
   BandRows_Label.SetFixedWidth( labelWidth1 );
   BandRows_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );
   BandRows_SpinBox.SetRange( 1, 65536 );
   BandRows_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_BandRowsValueUpdated, w );
   BandRows_SpinBox.SetFixedWidth( editWidth1 );
   BandRows_SpinBox.SetToolTip( "<p>Output rows per streamed band. Advanced: affects streaming granularity "
      "and peak memory only; the default (256) is fine for most images.</p>" );

   BandRows_Sizer.SetSpacing( 4 );
   BandRows_Sizer.Add( BandRows_Label );
   BandRows_Sizer.Add( BandRows_SpinBox );
   BandRows_Sizer.AddStretch();

   DefectVeto_CheckBox.SetText( "Cross-panel defect veto" );
   DefectVeto_CheckBox.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_DefectVetoClick, w );
   DefectVeto_CheckBox.SetToolTip( "<p>Reject single-panel defects (satellite trails, stacking edge artifacts) "
      "in overlap regions by cross-checking panels during the detail blend.</p>" );

   DefectVeto_Sizer.SetSpacing( 4 );
   DefectVeto_Sizer.AddUnscaledSpacing( labelWidth1 + w.LogicalPixelsToPhysical( 4 ) );
   DefectVeto_Sizer.Add( DefectVeto_CheckBox );
   DefectVeto_Sizer.AddStretch();

   SeamMap_CheckBox.SetText( "Seam map" );
   SeamMap_CheckBox.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_SeamMapClick, w );
   SeamMap_CheckBox.SetToolTip( "<p>Produce the seam/ownership map as a second output image: "
      "an autostretched preview of the blended mosaic with each panel's owned region tinted, "
      "seams drawn dark, and panel numbers at the region centers. Useful for checking where "
      "the seams were placed and which panel contributed each region.</p>" );

   SeamMap_Sizer.SetSpacing( 4 );
   SeamMap_Sizer.AddUnscaledSpacing( labelWidth1 + w.LogicalPixelsToPhysical( 4 ) );
   SeamMap_Sizer.Add( SeamMap_CheckBox );
   SeamMap_Sizer.AddStretch();

   FlattenEnabled_CheckBox.SetText( "Gradient removal, order:" );
   FlattenEnabled_CheckBox.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_FlattenEnabledClick, w );
   FlattenEnabled_CheckBox.SetToolTip( "<p>Remove the residual global background gradient from the "
      "merged result, fitting a polynomial of the chosen order. The central background level "
      "is preserved.</p>" );

   FlattenOrder_SpinBox.SetRange( 1, 2 );
   FlattenOrder_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_FlattenOrderValueUpdated, w );
   FlattenOrder_SpinBox.SetFixedWidth( editWidth1 );
   FlattenOrder_SpinBox.SetToolTip( "<p>Gradient removal polynomial order: 1 = plane, 2 = quadratic.</p>" );

   Flatten_Sizer.SetSpacing( 4 );
   Flatten_Sizer.AddUnscaledSpacing( labelWidth1 + w.LogicalPixelsToPhysical( 4 ) );
   Flatten_Sizer.Add( FlattenEnabled_CheckBox );
   Flatten_Sizer.Add( FlattenOrder_SpinBox );
   Flatten_Sizer.AddStretch();

   Parameters_Sizer.SetSpacing( 4 );
   Parameters_Sizer.Add( InputSelect_Sizer );
   Parameters_Sizer.Add( BlendMode_Sizer );
   Parameters_Sizer.Add( Feather_NumericControl );
   Parameters_Sizer.Add( SurfaceOrder_Sizer );
   Parameters_Sizer.Add( GainMode_Sizer );
   Parameters_Sizer.Add( DefectVeto_Sizer );
   Parameters_Sizer.Add( SeamMap_Sizer );
   Parameters_Sizer.Add( Flatten_Sizer );
   Parameters_Control.SetSizer( Parameters_Sizer );

   Parameters_SectionBar.SetTitle( "Parameters" );
   Parameters_SectionBar.SetSection( Parameters_Control );
   Parameters_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );

   //
   // Advanced section (collapsed by default).
   //
   Advanced_Sizer.SetSpacing( 4 );
   Advanced_Sizer.Add( SessionDir_Sizer );
   Advanced_Sizer.Add( BandRows_Sizer );
   Advanced_Control.SetSizer( Advanced_Sizer );

   Advanced_SectionBar.SetTitle( "Advanced" );
   Advanced_SectionBar.SetSection( Advanced_Control );
   Advanced_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );

   //
   // Top-level layout.
   //
   Global_Sizer.SetMargin( 8 );
   Global_Sizer.SetSpacing( 6 );
   Global_Sizer.Add( Notice_Control );
   Global_Sizer.Add( TargetFrames_SectionBar );
   Global_Sizer.Add( TargetFrames_Control );
   Global_Sizer.Add( Parameters_SectionBar );
   Global_Sizer.Add( Parameters_Control );
   Global_Sizer.Add( Advanced_SectionBar );
   Global_Sizer.Add( Advanced_Control );

   w.SetSizer( Global_Sizer );

   // Start collapsed. Control::Hide() on the section control -- the standard
   // PCL module idiom -- NOT SectionBar::HideSection(): SetSectionVisible()
   // early-outs when the section already reports IsVisible() == false, which
   // is always the case here because the window has never been shown, so
   // HideSection() at construction time is a silent no-op and the section
   // would come up expanded. The bar's arrow starts in the collapsed state
   // (SetSection() samples IsVisible() pre-show) and its OnShow/OnHide
   // tracking keeps it in sync from then on.
   Advanced_Control.Hide();

   w.EnsureLayoutUpdated();
   w.AdjustToContents();
}

// ----------------------------------------------------------------------------
// Header notice: logo paint + hyperlink labels.
// ----------------------------------------------------------------------------

void MmmBlendInterface::e_LogoPaint( Control& sender, const pcl::Rect& )
{
   Graphics g( sender );
   if ( !GUI->Logo_Bitmap.IsNull() )
      g.DrawScaledBitmap( sender.BoundsRect(), GUI->Logo_Bitmap );
}

void MmmBlendInterface::e_LinkMouseRelease( Control& sender, const pcl::Point&, int button, unsigned, unsigned )
{
   if ( button != MouseButton::Left )
      return;

   String url;
   if ( &sender == &GUI->ShareLink_Label )
      url = "https://astrometrical.com";
   else if ( &sender == &GUI->ToolsLink_Label )
      url = "https://tools.astrometrical.com/";
   else if ( &sender == &GUI->KofiLink_Label )
      url = "https://ko-fi.com/astrometrical";
   else
      return;

   try
   {
#ifdef _WIN32
      ExternalProcess::StartProgram( "cmd.exe", StringList() << "/c" << "start" << "" << url );
#elif defined( __APPLE__ )
      ExternalProcess::StartProgram( "open", StringList() << url );
#else
      ExternalProcess::StartProgram( "xdg-open", StringList() << url );
#endif
   }
   catch ( ... )
   {
      // Non-fatal: each link's tooltip shows the full URL anyway.
   }
}

// ----------------------------------------------------------------------------
// SectionBar toggle: fix tree box heights while animating, restore afterwards,
// and let the window shrink when a section closes.
// ----------------------------------------------------------------------------

void MmmBlendInterface::e_ToggleSection( SectionBar&, Control& section, bool start )
{
   if ( start )
   {
      GUI->Views_TreeBox.SetFixedHeight();
      GUI->Files_TreeBox.SetFixedHeight();
   }
   else
   {
      GUI->Views_TreeBox.SetScaledMinHeight( 120 );
      GUI->Views_TreeBox.SetMaxHeight( int_max );
      GUI->Files_TreeBox.SetScaledMinHeight( 120 );
      GUI->Files_TreeBox.SetMaxHeight( int_max );
      if ( GUI->TargetFrames_Control.IsVisible() )
         SetVariableHeight();
      else
         SetFixedHeight();
      AdjustToContents();
   }
}

// ----------------------------------------------------------------------------
// Control <-> instance sync.
// ----------------------------------------------------------------------------

void MmmBlendInterface::UpdateControls()
{
   GUI->ViewsMode_RadioButton.SetChecked( m_viewsMode );
   GUI->FilesMode_RadioButton.SetChecked( !m_viewsMode );
   UpdateInputModeControls();

   PopulateActiveTreeBox();

   GUI->SessionDir_Edit.SetText( m_instance.p_sessionDir );

   GUI->InputSelect_ComboBox.SetCurrentItem( m_instance.p_inputSelect );
   GUI->BlendMode_ComboBox.SetCurrentItem( m_instance.p_blendMode );

   GUI->Feather_NumericControl.SetValue( m_instance.p_feather );
   GUI->SurfaceOrder_SpinBox.SetValue( m_instance.p_surfaceOrder );
   GUI->GainMode_ComboBox.SetCurrentItem( m_instance.p_gainMode );
   GUI->BandRows_SpinBox.SetValue( m_instance.p_bandRows );

   GUI->DefectVeto_CheckBox.SetChecked( m_instance.p_defectVeto );
   GUI->SeamMap_CheckBox.SetChecked( m_instance.p_seamMap );

   GUI->FlattenEnabled_CheckBox.SetChecked( m_instance.p_flattenEnabled );
   GUI->FlattenOrder_SpinBox.SetValue( m_instance.p_flatten );
   UpdateFlattenControls();
}

void MmmBlendInterface::UpdateInputModeControls()
{
   // Show only the active side's list; hiding (not disabling) matches the
   // reference tools and keeps the window compact.
   GUI->Views_TreeBox.SetVisible( m_viewsMode );
   GUI->AddViews_PushButton.SetVisible( m_viewsMode );
   GUI->RemoveView_PushButton.SetVisible( m_viewsMode );

   GUI->Files_TreeBox.SetVisible( !m_viewsMode );
   GUI->AddFiles_PushButton.SetVisible( !m_viewsMode );
   GUI->RemoveFile_PushButton.SetVisible( !m_viewsMode );

   EnsureLayoutUpdated();
   AdjustToContents();
}

void MmmBlendInterface::UpdateFlattenControls()
{
   GUI->FlattenOrder_SpinBox.Enable( m_instance.p_flattenEnabled );
}

Array<String>& MmmBlendInterface::ActiveGroups()
{
   return m_viewsMode ? m_instance.p_viewGroups : m_instance.p_fileGroups;
}

Array<String>& MmmBlendInterface::ActiveItems()
{
   return m_viewsMode ? m_instance.p_viewIds : m_instance.p_filePaths;
}

// Display text matched by the filter: the view id, or the file NAME (no dir).
static String DisplayText( bool viewsMode, const String& item )
{
   return viewsMode ? item : File::ExtractNameAndSuffix( item );
}

// Rebuilds the active list from the instance arrays, showing only the rows
// that match the filter pattern; m_visibleRows records the mapping back to
// instance rows. The group array is kept in lockstep with the item array
// (icons from older versions, or the add/remove paths, can leave it short).
void MmmBlendInterface::PopulateActiveTreeBox()
{
   TreeBox& tree = m_viewsMode ? GUI->Views_TreeBox : GUI->Files_TreeBox;
   tree.Clear();
   m_visibleRows.Clear();
   Array<String>& items  = ActiveItems();
   Array<String>& groups = ActiveGroups();
   while ( groups.Length() < items.Length() )
      groups.Add( String() );
   while ( groups.Length() > items.Length() )
      groups.Remove( groups.At( groups.Length() - 1 ) );
   const std::string pattern( m_filterPattern.ToUTF8().c_str() );
   for ( size_type i = 0; i < items.Length(); ++i )
   {
      const String text = DisplayText( m_viewsMode, items[i] );
      if ( !pattern.empty()
        && !mmm_groups::wildcard_match( pattern, std::string( text.ToUTF8().c_str() ) ) )
         continue;
      TreeBox::Node* node = new TreeBox::Node( tree );
      node->SetText( 0, items[i] );
      node->SetText( 1, groups[i] );
      m_visibleRows.Add( int( i ) );
   }
   tree.AdjustColumnWidthToContents( 0 );
}

Array<int> MmmBlendInterface::TargetRows() const
{
   TreeBox& tree = m_viewsMode ? GUI->Views_TreeBox : GUI->Files_TreeBox;
   IndirectArray<TreeBox::Node> selected = tree.SelectedNodes();
   if ( selected.IsEmpty() )
      return m_visibleRows;   // nothing selected: every displayed row
   Array<int> rows;
   for ( TreeBox::Node* node : selected )
   {
      int idx = tree.ChildIndex( node );
      if ( idx >= 0 && size_type( idx ) < m_visibleRows.Length() )
         rows.Add( m_visibleRows[idx] );
   }
   return rows;
}

void MmmBlendInterface::ShowFilterHint( bool show )
{
   m_filterHintShown = show;
   if ( show )
   {
      GUI->Filter_Edit.SetText( kFilterHint );
      GUI->Filter_Edit.SetStyleSheet( "QLineEdit { color: #808080; font-style: italic; }" );
   }
   else
   {
      if ( GUI->Filter_Edit.Text() == kFilterHint )
         GUI->Filter_Edit.Clear();
      GUI->Filter_Edit.SetStyleSheet( String() );
   }
}

// ----------------------------------------------------------------------------
// Event handlers.
// ----------------------------------------------------------------------------

void MmmBlendInterface::e_ModeClick( Button& sender, bool checked )
{
   // Only react to the button that has just turned ON; this keeps behavior
   // correct whether or not the platform's RadioButton grouping also fires a
   // (checked=false) event on the sibling button.
   if ( !checked )
      return;

   if ( &sender == &GUI->ViewsMode_RadioButton )
   {
      m_viewsMode = true;
      m_instance.p_filePaths.Clear();
      m_instance.p_fileGroups.Clear();
   }
   else if ( &sender == &GUI->FilesMode_RadioButton )
   {
      m_viewsMode = false;
      m_instance.p_viewIds.Clear();
      m_instance.p_viewGroups.Clear();
   }

   UpdateControls();
}

void MmmBlendInterface::e_AddViewsClick( Button&, bool )
{
   MultiViewSelectionDialog d;
   if ( d.Execute() == StdDialogCode::Ok )
   {
      for ( const View& v : d.Views() )
      {
         String id( v.FullId().UTF8ToUTF16() );
         bool exists = false;
         for ( const String& s : m_instance.p_viewIds )
            if ( s == id )
            {
               exists = true;
               break;
            }
         if ( !exists )
         {
            m_instance.p_viewIds.Add( id );
            m_instance.p_viewGroups.Add( String() );
         }
      }

      // Adding views implies Views mode (mutual exclusion, spec 10.1).
      m_instance.p_filePaths.Clear();
      m_instance.p_fileGroups.Clear();
      m_viewsMode = true;

      UpdateControls();
   }
}

void MmmBlendInterface::e_RemoveViewClick( Button&, bool )
{
   IndirectArray<TreeBox::Node> selected = GUI->Views_TreeBox.SelectedNodes();

   // Node indices map to instance rows through m_visibleRows (the list may
   // be filtered). Remove from the highest row down so earlier rows stay
   // valid, keeping the group column in lockstep.
   Array<int> rows;
   for ( TreeBox::Node* node : selected )
   {
      int idx = GUI->Views_TreeBox.ChildIndex( node );
      if ( idx >= 0 && size_type( idx ) < m_visibleRows.Length() )
         rows.Add( m_visibleRows[idx] );
   }
   rows.Sort();
   for ( int i = int( rows.Length() ) - 1; i >= 0; --i )
   {
      int row = rows[i];
      if ( row >= 0 && size_type( row ) < m_instance.p_viewIds.Length() )
      {
         m_instance.p_viewIds.Remove( m_instance.p_viewIds.Begin() + row );
         if ( size_type( row ) < m_instance.p_viewGroups.Length() )
            m_instance.p_viewGroups.Remove( m_instance.p_viewGroups.Begin() + row );
      }
   }

   UpdateControls();
}

void MmmBlendInterface::e_AddFilesClick( Button&, bool )
{
   OpenFileDialog d;
   d.LoadImageFilters();
   d.EnableMultipleSelections();
   if ( d.Execute() )
   {
      for ( const String& f : d.FileNames() )
      {
         bool exists = false;
         for ( const String& s : m_instance.p_filePaths )
            if ( s == f )
            {
               exists = true;
               break;
            }
         if ( !exists )
         {
            m_instance.p_filePaths.Add( f );
            m_instance.p_fileGroups.Add( String() );
         }
      }

      // Adding files implies Files mode (mutual exclusion, spec 10.1).
      m_instance.p_viewIds.Clear();
      m_instance.p_viewGroups.Clear();
      m_viewsMode = false;

      UpdateControls();
   }
}

void MmmBlendInterface::e_RemoveFileClick( Button&, bool )
{
   IndirectArray<TreeBox::Node> selected = GUI->Files_TreeBox.SelectedNodes();

   Array<int> rows;
   for ( TreeBox::Node* node : selected )
   {
      int idx = GUI->Files_TreeBox.ChildIndex( node );
      if ( idx >= 0 && size_type( idx ) < m_visibleRows.Length() )
         rows.Add( m_visibleRows[idx] );
   }
   rows.Sort();
   for ( int i = int( rows.Length() ) - 1; i >= 0; --i )
   {
      int row = rows[i];
      if ( row >= 0 && size_type( row ) < m_instance.p_filePaths.Length() )
      {
         m_instance.p_filePaths.Remove( m_instance.p_filePaths.Begin() + row );
         if ( size_type( row ) < m_instance.p_fileGroups.Length() )
            m_instance.p_fileGroups.Remove( m_instance.p_fileGroups.Begin() + row );
      }
   }

   UpdateControls();
}

void MmmBlendInterface::e_FilterGetFocus( Control& )
{
   if ( m_filterHintShown )
      ShowFilterHint( false );
}

void MmmBlendInterface::e_FilterLoseFocus( Control& )
{
   if ( GUI->Filter_Edit.Text().IsEmpty() )
      ShowFilterHint( true );
}

void MmmBlendInterface::e_FilterTextUpdated( Edit&, const String& text )
{
   if ( m_filterHintShown )
      return;   // programmatic hint text, not a pattern
   m_filterPattern = text.Trimmed();
   PopulateActiveTreeBox();
}

void MmmBlendInterface::e_SetGroupClick( Button&, bool )
{
   const String name = GUI->Group_Edit.Text().Trimmed();
   Array<String>& groups = ActiveGroups();
   for ( int row : TargetRows() )
      if ( row >= 0 && size_type( row ) < groups.Length() )
         groups[row] = name;
   PopulateActiveTreeBox();
}

void MmmBlendInterface::e_ClearGroupsClick( Button&, bool )
{
   Array<String>& groups = ActiveGroups();
   for ( int row : TargetRows() )
      if ( row >= 0 && size_type( row ) < groups.Length() )
         groups[row].Clear();
   PopulateActiveTreeBox();
}

// FILTER value of an open view: the FITS keyword array, quotes/padding
// stripped; empty when absent.
static String ViewFilterName( const String& viewId )
{
   View v = View::ViewById( viewId );
   if ( v.IsNull() )
      return String();
   FITSKeywordArray keywords = v.Window().Keywords();
   for ( const FITSHeaderKeyword& k : keywords )
      if ( k.name == "FILTER" )
         return String( k.StripValueDelimiters() ).Trimmed();
   return String();
}

// RAII: disables the interface (no clicks reach the live instance while a
// pumped worker probe runs) and clears the re-entrancy flag on every exit.
struct ProbeScopeGuard
{
   MmmBlendInterface& iface;
   bool&              flag;
   ProbeScopeGuard( MmmBlendInterface& i, bool& f ) : iface( i ), flag( f )
   {
      flag = true;
      iface.Disable();
   }
   ~ProbeScopeGuard()
   {
      iface.Enable();
      flag = false;
   }
};

void MmmBlendInterface::e_GroupByFilterClick( Button&, bool )
{
   if ( m_probeInProgress )
      return;   // a click delivered by the probe's own event pump
   Array<int> rows = TargetRows();
   Array<String>& items  = ActiveItems();
   Array<String>& groups = ActiveGroups();
   int missing = 0;
   if ( m_viewsMode )
   {
      for ( int row : rows )
      {
         String f = ViewFilterName( items[row] );
         if ( f.IsEmpty() )
            ++missing;
         else
            groups[row] = f;
      }
   }
   else
   {
      // Header pass in the worker process (never on this thread): one
      // --probe-panels over the targeted files, pumped like a run's probe.
      std::vector<std::string> paths;
      for ( int row : rows )
         paths.push_back( std::string( items[row].ToUTF8().c_str() ) );
      if ( !paths.empty() )
      {
         try
         {
            ProbeScopeGuard scope( *this, m_probeInProgress );
            Console().EnableAbort();
            mmm::PanelProbeResult probe = probe_filter_names( paths );
            // The probe pumped the event queue; re-validate every row against
            // the live arrays before writing (belt and braces beside the
            // disabled interface).
            for ( size_type i = 0; i < rows.Length() && i < probe.panels.size(); ++i )
            {
               const int row = rows[i];
               if ( row < 0 || size_type( row ) >= groups.Length() || size_type( row ) >= items.Length() )
                  continue;
               if ( std::string( items[row].ToUTF8().c_str() ) != paths[i] )
                  continue;
               if ( probe.panels[i].filter.empty() )
                  ++missing;
               else
                  groups[row] = String( IsoString( probe.panels[i].filter.c_str() ).UTF8ToUTF16() );
            }
         }
         catch ( const mmm::HostCancelled& )
         {
            return;
         }
         catch ( const mmm::HostError& e )
         {
            throw Error( String( "MegaMergeMosaic: could not read FILTER keywords: " ) + e.what() );
         }
      }
   }
   if ( missing > 0 )
      Console().WarningLn( String().Format( "<end><cbr>** MegaMergeMosaic: %d panel(s) carry no FILTER "
                                            "keyword; their group was not changed.", missing ) );
   PopulateActiveTreeBox();
}

void MmmBlendInterface::e_SessionDirEditCompleted( Edit& sender )
{
   m_instance.p_sessionDir = sender.Text();
}

void MmmBlendInterface::e_SessionDirBrowseClick( Button&, bool )
{
   GetDirectoryDialog d;
   d.SetCaption( "Mega Merge Mosaic: Select Session Directory" );
   if ( d.Execute() )
   {
      m_instance.p_sessionDir = d.Directory();
      UpdateControls();
   }
}

void MmmBlendInterface::e_InputSelectItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_inputSelect = pcl_enum( itemIndex );
}

void MmmBlendInterface::e_BlendModeItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_blendMode = pcl_enum( itemIndex );
}

void MmmBlendInterface::e_FeatherValueUpdated( NumericEdit&, double value )
{
   m_instance.p_feather = int32( value );
}

void MmmBlendInterface::e_SurfaceOrderValueUpdated( SpinBox&, int value )
{
   m_instance.p_surfaceOrder = int32( value );
}

void MmmBlendInterface::e_GainModeItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_gainMode = pcl_enum( itemIndex );
}

void MmmBlendInterface::e_BandRowsValueUpdated( SpinBox&, int value )
{
   m_instance.p_bandRows = int32( value );
}

void MmmBlendInterface::e_DefectVetoClick( Button&, bool checked )
{
   m_instance.p_defectVeto = checked;
}

void MmmBlendInterface::e_SeamMapClick( Button&, bool checked )
{
   m_instance.p_seamMap = checked;
}

void MmmBlendInterface::e_FlattenEnabledClick( Button&, bool checked )
{
   m_instance.p_flattenEnabled = checked;
   UpdateFlattenControls();
}

void MmmBlendInterface::e_FlattenOrderValueUpdated( SpinBox&, int value )
{
   m_instance.p_flatten = int32( value );
}

// ----------------------------------------------------------------------------

} // namespace pcl
